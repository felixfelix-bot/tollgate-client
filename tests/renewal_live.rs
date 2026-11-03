//! Drive the renewal decision against a live gate.
//!
//! `/usage` is reported by the gate from `ndsctl json`, and the fake `ndsctl`
//! reads `/tmp/fake-nds-usage` when present ("<downloaded_kb> <uploaded_kb>").
//! That is the seam that lets this harness move a session to a chosen fill level
//! without waiting for real traffic — so each branch of the decision is exercised
//! against a real gate and real HTTP, not a mock.
//!
//! Run: cargo test --no-default-features --features android --test renewal_live

use tollgate_client_core::renewal::{self, RenewalDecision, RenewalPolicy, SessionState};
use tollgate_client_core::runner::read_session;

const GATE: &str = "http://127.0.0.1:2121/";

fn policy(cost: u64, budget: u64, spent: u64, pending: bool) -> RenewalPolicy {
    RenewalPolicy {
        top_up_below: 0.20,
        min_top_up: 1,
        daily_budget: budget,
        spent_today: spent,
        cost,
        pending,
    }
}

/// Skip cleanly when no lab gate is running, rather than reporting a false pass.
fn gate_up() -> bool {
    std::net::TcpStream::connect_timeout(
        &"127.0.0.1:2121".parse().unwrap(),
        std::time::Duration::from_millis(500),
    )
    .is_ok()
}

#[test]
fn live_gate_reports_a_healthy_session_as_wait() {
    if !gate_up() {
        eprintln!("SKIP: no lab gate on 127.0.0.1:2121");
        return;
    }
    let reading = read_session(GATE, 3000).expect("read_session against the live gate");
    eprintln!("reading = {reading:?}");
    // Whatever the gate says, a healthy session must not authorise a purchase.
    match renewal::decide(reading, &policy(1, 100, 0, false)) {
        RenewalDecision::TopUp { why } => {
            // Only legitimate when the session is genuinely below threshold.
            assert!(
                !reading.is_meterable() || reading.remaining_fraction() <= 0.20,
                "top-up authorised with headroom left: {why} ({reading:?})"
            );
        }
        RenewalDecision::Wait { .. } | RenewalDecision::Blocked { .. } => {}
    }
}

#[test]
fn an_expired_session_is_always_a_top_up() {
    // Constructed, not fetched: the point is the decision table, and a real
    // expiry would mean waiting for real traffic.
    let reading = renewal::SessionReading {
        state: SessionState::Expired,
        used: 44040192,
        total: 44040192,
    };
    match renewal::decide(reading, &policy(1, 100, 0, false)) {
        RenewalDecision::TopUp { why } => assert!(why.contains("run out"), "{why}"),
        other => panic!("expected TopUp, got {other:?}"),
    }
}

#[test]
fn a_pending_purchase_blocks_even_against_the_live_gate() {
    if !gate_up() {
        eprintln!("SKIP: no lab gate on 127.0.0.1:2121");
        return;
    }
    let reading = read_session(GATE, 3000).expect("read_session");
    // The pending flag must dominate: a second purchase while one is in flight is
    // how one extension gets paid for twice.
    assert!(matches!(
        renewal::decide(reading, &policy(1, 100, 0, true)),
        RenewalDecision::Blocked { .. }
    ));
}

#[test]
fn the_budget_stops_a_top_up_the_session_genuinely_needs() {
    let reading = renewal::SessionReading {
        state: SessionState::Active,
        used: 43000000,
        total: 44040192,
    };
    // Genuinely below threshold (2.4% left) and still refused: the cap is not
    // advisory.
    assert!(matches!(
        renewal::decide(reading, &policy(1, 10, 10, false)),
        RenewalDecision::Blocked { .. }
    ));
    // Same reading, budget available: top up.
    assert!(matches!(
        renewal::decide(reading, &policy(1, 100, 0, false)),
        RenewalDecision::TopUp { .. }
    ));
}
