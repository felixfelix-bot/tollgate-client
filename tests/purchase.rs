//! Spec for the v1 payment exchange and its crash-safety state machine.
//!
//! Wire contract, from tollgate-module-basic-go `src/main.go`:
//!   POST / with the raw Cashu token as the body ("Content-Type: text/plain")
//!   200 + kind 1022  -> session granted
//!   400 + kind 21023 -> refusal (level/code/message tags)
//!   500 + kind 21023 -> internal error: the token MAY have been consumed
//!
//! The distinction that matters is definitive-refusal vs ambiguous. Treating a
//! 500 or a timeout as a refusal is how a customer pays twice.

use tollgate_client_core::purchase::{
    classify_response, intent_key, next_state, IntentState, PaymentOutcome, TransitionError,
};

fn session_event() -> String {
    r#"{"kind":1022,"tags":[["metric","milliseconds"],["allotment","60000"]]}"#.to_string()
}

fn notice_event(code: &str, msg: &str) -> String {
    format!(r#"{{"kind":21023,"tags":[["level","error"],["code","{code}"],["message","{msg}"]]}}"#)
}

#[test]
fn a_200_with_a_session_event_is_a_grant() {
    assert_eq!(
        classify_response(200, &session_event()),
        PaymentOutcome::Granted {
            allotment: Some(60_000),
            metric: Some("milliseconds".to_string())
        }
    );
}

#[test]
fn a_400_with_a_notice_event_is_a_definitive_refusal() {
    match classify_response(400, &notice_event("payment-error-invalid-token", "bad token")) {
        PaymentOutcome::Refused { code, message } => {
            assert_eq!(code, "payment-error-invalid-token");
            assert_eq!(message, "bad token");
        }
        other => panic!("expected Refused, got {other:?}"),
    }
}

/// A 500 may mean the mint took the note and the answer was lost. Never a refusal.
#[test]
fn a_500_is_ambiguous_not_a_refusal() {
    assert!(matches!(
        classify_response(500, &notice_event("internal-error", "Payment processing failed")),
        PaymentOutcome::Ambiguous { .. }
    ));
}

/// A 200 we cannot parse proves nothing — do not book a grant we cannot see.
#[test]
fn a_200_with_an_unparseable_body_is_ambiguous() {
    assert!(matches!(
        classify_response(200, "not json at all"),
        PaymentOutcome::Ambiguous { .. }
    ));
    assert!(matches!(
        classify_response(200, ""),
        PaymentOutcome::Ambiguous { .. }
    ));
}

/// A 400 we cannot parse could be a proxy error, not the gate's refusal.
#[test]
fn a_400_with_an_unparseable_body_is_ambiguous() {
    assert!(matches!(
        classify_response(400, "<html>502 Bad Gateway</html>"),
        PaymentOutcome::Ambiguous { .. }
    ));
}

#[test]
fn a_200_carrying_a_notice_event_is_not_a_grant() {
    assert!(matches!(
        classify_response(200, &notice_event("client-not-registered", "no session")),
        PaymentOutcome::Refused { .. } | PaymentOutcome::Ambiguous { .. }
    ));
}

#[test]
fn states_move_forward_only() {
    assert_eq!(
        next_state(IntentState::Pending, &PaymentOutcome::Granted { allotment: None, metric: None }).unwrap(),
        IntentState::Granted
    );
    assert_eq!(
        next_state(IntentState::Pending, &PaymentOutcome::Ambiguous { reason: "timeout".into() }).unwrap(),
        IntentState::Ambiguous
    );
    // A grant is terminal even if a duplicate answer says otherwise.
    assert_eq!(
        next_state(IntentState::Granted, &PaymentOutcome::Ambiguous { reason: "late".into() }).unwrap(),
        IntentState::Granted
    );
    // You cannot un-refuse a definitive refusal into a grant.
    assert!(matches!(
        next_state(IntentState::Refused, &PaymentOutcome::Granted { allotment: None, metric: None }),
        Err(TransitionError { .. })
    ));
}

/// Reconciliation of an ambiguous attempt comes from the mint, not from hope.
#[test]
fn ambiguous_reconciles_to_spent_or_unspent() {
    assert_eq!(
        next_state(IntentState::Ambiguous, &PaymentOutcome::Reconciled { spent: true }).unwrap(),
        IntentState::Spent
    );
    assert_eq!(
        next_state(IntentState::Ambiguous, &PaymentOutcome::Reconciled { spent: false }).unwrap(),
        IntentState::Unspent
    );
    // Reconciling something that was never ambiguous is a bug, not a no-op.
    assert!(next_state(IntentState::Pending, &PaymentOutcome::Reconciled { spent: true }).is_err());
}

/// The journal key must identify an attempt without storing the bearer token.
#[test]
fn the_intent_key_is_stable_and_does_not_contain_the_token() {
    let token = "cashuBexampleTOKENvalue123";
    let a = intent_key(token);
    let b = intent_key(token);
    assert_eq!(a, b);
    assert_ne!(a, intent_key("cashuBdifferentTOKENvalue"));
    assert!(!a.contains("cashuB"));
    assert_eq!(a.len(), 16, "expect 16 hex chars");
}
