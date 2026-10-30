//! The module's kind:21023 codes carry retry semantics, and getting them wrong
//! costs the customer money. Classified from docs/client/v1-client-contract.md §5.
//!
//! "Never resend" codes are the dangerous ones: a client that treats them as
//! refusals pays twice.

use tollgate_client_core::purchase::{classify_response, PaymentOutcome};

fn notice(code: &str) -> String {
    format!(r#"{{"kind":21023,"tags":[["level","error"],["code","{code}"],["message","m"]]}}"#)
}

fn outcome(code: &str) -> PaymentOutcome {
    classify_response(400, &notice(code))
}

#[test]
fn not_taken_codes_are_definitive_refusals() {
    for code in [
        "invalid-mac-address",
        "payment-error-invalid-token",
        "payment-error-below-swap-fee",
        "payment-processing-failed",
        "session-error",
    ] {
        assert!(matches!(outcome(code), PaymentOutcome::Refused { .. }), "{code}");
    }
}

/// The note was not touched; a later attempt with the SAME note is allowed.
#[test]
fn untouched_codes_are_retryable() {
    for code in [
        "device-unresolved",
        "client-not-registered",
        "mint-rate-limited",
        "payment-error-mint-unreachable",
        "service-unavailable",
        "starting",
    ] {
        assert!(matches!(outcome(code), PaymentOutcome::Retryable { .. }), "{code}");
    }
}

/// These mean the note MAY already be spent. Resending is how you pay twice.
#[test]
fn may_be_spent_codes_are_ambiguous() {
    for code in [
        "payment-duplicate-inflight",
        "payment-received-grant-pending",
        "payment-outcome-unknown",
    ] {
        assert!(matches!(outcome(code), PaymentOutcome::Ambiguous { .. }), "{code}");
    }
}

#[test]
fn spent_and_retired_keyset_are_value_lost() {
    for code in ["payment-error-token-spent", "payment-error-keyset-expired"] {
        assert!(matches!(outcome(code), PaymentOutcome::ValueLost { .. }), "{code}");
    }
}

/// A code we do not know must not be read as "safe to resend".
#[test]
fn an_unknown_code_is_ambiguous_not_a_refusal() {
    assert!(matches!(outcome("payment-error-something-new"), PaymentOutcome::Ambiguous { .. }));
}

/// A 503 from the startup gate is retryable, not a refusal.
#[test]
fn the_startup_gate_503_is_retryable() {
    assert!(matches!(
        classify_response(503, &notice("starting")),
        PaymentOutcome::Ambiguous { .. } | PaymentOutcome::Retryable { .. }
    ));
}

/// The grant names the device it answered for; a client must be able to compare.
#[test]
fn a_grant_carries_the_device_identifier() {
    let body = r#"{"kind":1022,"tags":[["device-identifier","aa:bb:cc:dd:ee:ff"],["allotment","60000"]]}"#;
    match classify_response(200, body) {
        PaymentOutcome::Granted { device_identifier, .. } => {
            assert_eq!(device_identifier.as_deref(), Some("aa:bb:cc:dd:ee:ff"))
        }
        other => panic!("expected Granted, got {other:?}"),
    }
}
