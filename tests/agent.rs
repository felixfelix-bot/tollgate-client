//! Spec for one purchase attempt, end to end, with fakes standing in for the
//! wallet, the gate and the journal.
//!
//! The money rules this pins down:
//! 1. caps are checked BEFORE a token is minted — never mint what you may not spend;
//! 2. a duplicate attempt is never re-posted (the same note spent twice is a loss);
//! 3. an ambiguous answer is reconciled against the mint, never retried blindly.

use std::collections::HashMap;
use tollgate_client_core::agent::{attempt_purchase, AttemptResult, GateTransport, Journal, Wallet};
use tollgate_client_core::policy::Caps;
use tollgate_client_core::purchase::{IntentState, PaymentOutcome};
use tollgate_client_core::scoring::{Metric, Offer};

fn offer() -> Offer {
    Offer {
        gateway_id: "gw-1".to_string(),
        mint_url: "https://mint.example".to_string(),
        unit: "sat".to_string(),
        metric: Metric::Milliseconds,
        price_per_step: 2,
        step_size: 60_000,
        min_steps: 1,
    }
}

fn caps() -> Caps {
    Caps {
        preferred_unit: "sat".to_string(),
        max_cost_per_minute: 10.0,
        daily_budget: 1_000.0,
        spent_today: 0.0,
        switch_margin: 0.20,
    }
}

#[derive(Default)]
struct FakeWallet {
    mint_calls: u32,
    spend_checks: u32,
    spent: bool,
    fail_mint: bool,
}

impl Wallet for FakeWallet {
    fn mint_token(&mut self, _mint: &str, _unit: &str, _amount: u64) -> Result<String, String> {
        self.mint_calls += 1;
        if self.fail_mint {
            return Err("mint unreachable".into());
        }
        Ok("cashuBfake".to_string())
    }
    fn is_spent(&mut self, _token: &str) -> Result<bool, String> {
        self.spend_checks += 1;
        Ok(self.spent)
    }
}

struct FakeTransport {
    post_calls: u32,
    response: Result<(u16, String), String>,
}

impl GateTransport for FakeTransport {
    fn post_token(&mut self, _gate: &str, _token: &str) -> Result<(u16, String), String> {
        self.post_calls += 1;
        self.response.clone()
    }
}

#[derive(Default)]
struct FakeJournal {
    states: HashMap<String, IntentState>,
}

impl Journal for FakeJournal {
    fn begin(&mut self, key: &str) -> Result<(), String> {
        if self.states.contains_key(key) {
            return Err("already attempted".into());
        }
        self.states.insert(key.to_string(), IntentState::Pending);
        Ok(())
    }
    fn set_state(&mut self, key: &str, state: IntentState) -> Result<(), String> {
        self.states.insert(key.to_string(), state);
        Ok(())
    }
    fn state(&self, key: &str) -> Option<IntentState> {
        self.states.get(key).copied()
    }
}

fn granted() -> (u16, String) {
    (200, r#"{"kind":1022,"tags":[["allotment","60000"],["metric","milliseconds"]]}"#.to_string())
}

fn refused() -> (u16, String) {
    (400, r#"{"kind":21023,"tags":[["code","payment-error-invalid-token"],["message","bad token"]]}"#.to_string())
}

#[test]
fn no_token_is_minted_when_a_cap_blocks_the_purchase() {
    let mut wallet = FakeWallet::default();
    let mut transport = FakeTransport { post_calls: 0, response: Ok(granted()) };
    let mut journal = FakeJournal::default();
    let mut c = caps();
    c.max_cost_per_minute = 0.5; // offer costs 2.0/min

    let r = attempt_purchase("gw-1", &offer(), &c, "attempt-1", &mut wallet, &mut transport, &mut journal).unwrap();
    assert!(matches!(r, AttemptResult::NotAttempted { .. }), "got {r:?}");
    assert_eq!(wallet.mint_calls, 0, "minted a token the caps forbade");
    assert_eq!(transport.post_calls, 0);
}

#[test]
fn a_grant_is_recorded_with_its_cost() {
    let mut wallet = FakeWallet::default();
    let mut transport = FakeTransport { post_calls: 0, response: Ok(granted()) };
    let mut journal = FakeJournal::default();

    let r = attempt_purchase("gw-1", &offer(), &caps(), "attempt-1", &mut wallet, &mut transport, &mut journal).unwrap();
    match r {
        AttemptResult::Finished { state, cost, outcome, .. } => {
            assert_eq!(state, IntentState::Granted);
            assert_eq!(cost, 2.0);
            assert!(matches!(outcome, PaymentOutcome::Granted { .. }));
        }
        other => panic!("expected Finished, got {other:?}"),
    }
    assert_eq!(journal.state(&tollgate_client_core::purchase::intent_key("attempt-1")), Some(IntentState::Granted));
}

#[test]
fn a_definitive_refusal_is_not_reconciled() {
    let mut wallet = FakeWallet::default();
    let mut transport = FakeTransport { post_calls: 0, response: Ok(refused()) };
    let mut journal = FakeJournal::default();

    let r = attempt_purchase("gw-1", &offer(), &caps(), "attempt-2", &mut wallet, &mut transport, &mut journal).unwrap();
    match r {
        AttemptResult::Finished { state, .. } => assert_eq!(state, IntentState::Refused),
        other => panic!("expected Finished, got {other:?}"),
    }
    assert_eq!(wallet.spend_checks, 0, "a plain refusal needs no mint query");
}

#[test]
fn a_timeout_reconciles_against_the_mint_and_books_a_spent_note() {
    let mut wallet = FakeWallet { spent: true, ..Default::default() };
    let mut transport = FakeTransport { post_calls: 0, response: Err("timeout".into()) };
    let mut journal = FakeJournal::default();

    let r = attempt_purchase("gw-1", &offer(), &caps(), "attempt-3", &mut wallet, &mut transport, &mut journal).unwrap();
    match r {
        AttemptResult::Finished { state, .. } => assert_eq!(state, IntentState::Spent),
        other => panic!("expected Finished, got {other:?}"),
    }
    assert_eq!(wallet.spend_checks, 1, "must ask the mint");
}

#[test]
fn a_timeout_with_an_unspent_note_is_retryable_but_not_auto_retried() {
    let mut wallet = FakeWallet { spent: false, ..Default::default() };
    let mut transport = FakeTransport { post_calls: 0, response: Err("timeout".into()) };
    let mut journal = FakeJournal::default();

    let r = attempt_purchase("gw-1", &offer(), &caps(), "attempt-4", &mut wallet, &mut transport, &mut journal).unwrap();
    match r {
        AttemptResult::Finished { state, .. } => assert_eq!(state, IntentState::Unspent),
        other => panic!("expected Finished, got {other:?}"),
    }
    assert_eq!(transport.post_calls, 1, "one attempt, not a retry loop");
}

#[test]
fn a_duplicate_attempt_is_never_reposted() {
    let mut wallet = FakeWallet::default();
    let mut transport = FakeTransport { post_calls: 0, response: Ok(granted()) };
    let mut journal = FakeJournal::default();
    let key = tollgate_client_core::purchase::intent_key("attempt-5");
    journal.states.insert(key.clone(), IntentState::Granted);

    let r = attempt_purchase("gw-1", &offer(), &caps(), "attempt-5", &mut wallet, &mut transport, &mut journal).unwrap();
    assert!(matches!(r, AttemptResult::AlreadyAttempted { .. }), "got {r:?}");
    assert_eq!(wallet.mint_calls, 0);
    assert_eq!(transport.post_calls, 0);
}

#[test]
fn a_failed_mint_spends_nothing() {
    let mut wallet = FakeWallet { fail_mint: true, ..Default::default() };
    let mut transport = FakeTransport { post_calls: 0, response: Ok(granted()) };
    let mut journal = FakeJournal::default();

    let r = attempt_purchase("gw-1", &offer(), &caps(), "attempt-6", &mut wallet, &mut transport, &mut journal);
    assert!(r.is_err(), "a mint failure must surface, not be swallowed");
    assert_eq!(transport.post_calls, 0, "nothing to post without a token");
    assert_eq!(journal.state(&tollgate_client_core::purchase::intent_key("attempt-6")), Some(IntentState::Unspent));
}
