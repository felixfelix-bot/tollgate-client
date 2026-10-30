//! The v1 payment exchange, and the only rule that matters: never confuse
//! "the gate said no" with "I did not hear the answer".
//!
//! Wire contract (tollgate-module-basic-go `src/main.go`):
//! - `POST /` with the raw Cashu token, `Content-Type: text/plain`
//! - `200` + kind `1022` → session granted (tags carry `allotment`, `metric`)
//! - `400` + kind `21023` → refusal, tags carry `code` and `message`
//! - `500` + kind `21023` → internal error: the token **may already be spent**
//!
//! A timeout, an unparseable 200, a 502 from a captive-portal proxy: all
//! ambiguous. The caller must reconcile against the mint (NUT-07) before it may
//! retry, because retrying a spent note is how a customer pays twice.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentOutcome {
    Granted {
        allotment: Option<u64>,
        metric: Option<String>,
    },
    Refused {
        code: String,
        message: String,
    },
    Ambiguous {
        reason: String,
    },
    /// The mint answered the ambiguity: the note is spent (grant is owed) or
    /// unspent (safe to retry).
    Reconciled { spent: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Pending,
    Granted,
    Refused,
    Ambiguous,
    Spent,
    Unspent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    pub reason: String,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}
impl std::error::Error for TransitionError {}

/// Stable, non-reversible handle for one payment attempt. The journal must never
/// hold the bearer token itself, and the operator must still be able to match a
/// support report to an attempt.
pub fn intent_key(token: &str) -> String {
    let mut h = DefaultHasher::new();
    "tollgate-intent-v1".hash(&mut h);
    token.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Nostr tags carry every value as a string, so `["allotment","60000"]` is the
/// real wire shape; a bare JSON number is also accepted for non-tag payloads.
fn number_after(hay: &str, key: &str) -> Option<u64> {
    let k = format!("\"{key}\"");
    let mut rest = &hay[hay.find(&k)? + k.len()..];
    rest = rest.trim_start_matches(|c: char| c == ':' || c == ',' || c == '[' || c.is_whitespace());
    if let Some(stripped) = rest.strip_prefix('"') {
        let digits: String = stripped.chars().take_while(|c| c.is_ascii_digit()).collect();
        return digits.parse().ok();
    }
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn string_after(hay: &str, key: &str) -> Option<String> {
    let k = format!("\"{key}\"");
    let rest = &hay[hay.find(&k)? + k.len()..];
    let open = rest.find('"')?;
    let after = &rest[open + 1..];
    let close = after.find('"')?;
    Some(after[..close].to_string())
}

/// Classify the gate's answer. `status` is the HTTP status; `body` is the
/// response body verbatim.
pub fn classify_response(status: u16, body: &str) -> PaymentOutcome {
    let kind = number_after(body, "kind");

    match (status, kind) {
        (200, Some(1022)) => PaymentOutcome::Granted {
            allotment: number_after(body, "allotment"),
            metric: string_after(body, "metric"),
        },
        (400, Some(21023)) => PaymentOutcome::Refused {
            code: string_after(body, "code").unwrap_or_else(|| "unknown".to_string()),
            message: string_after(body, "message").unwrap_or_default(),
        },
        // The gate's own internal error: it may have taken the note already.
        (s, Some(21023)) if s >= 500 => PaymentOutcome::Ambiguous {
            reason: format!(
                "gate answered {s} ({}); the note may be spent",
                string_after(body, "code").unwrap_or_else(|| "unknown".into())
            ),
        },
        (200, Some(21023)) => PaymentOutcome::Refused {
            code: string_after(body, "code").unwrap_or_else(|| "unknown".to_string()),
            message: string_after(body, "message").unwrap_or_default(),
        },
        _ => PaymentOutcome::Ambiguous {
            reason: format!("unclassifiable answer: HTTP {status}"),
        },
    }
}

/// Forward-only state machine for one attempt. Terminal states are sticky so a
/// duplicate or late answer cannot rewrite history.
pub fn next_state(
    current: IntentState,
    outcome: &PaymentOutcome,
) -> Result<IntentState, TransitionError> {
    use IntentState::*;
    match (current, outcome) {
        (Granted, _) | (Spent, _) => Ok(current),
        (Pending, PaymentOutcome::Granted { .. }) => Ok(Granted),
        (Pending, PaymentOutcome::Refused { .. }) => Ok(Refused),
        (Pending, PaymentOutcome::Ambiguous { .. }) => Ok(Ambiguous),
        (Pending, PaymentOutcome::Reconciled { .. }) => Err(TransitionError {
            reason: "nothing was ambiguous yet; reconciliation is not a purchase step".into(),
        }),
        (Refused, PaymentOutcome::Ambiguous { .. }) => Ok(Refused),
        (Refused, _) => Err(TransitionError {
            reason: "a definitive refusal is terminal".into(),
        }),
        (Ambiguous, PaymentOutcome::Reconciled { spent: true }) => Ok(Spent),
        (Ambiguous, PaymentOutcome::Reconciled { spent: false }) => Ok(Unspent),
        (Ambiguous, PaymentOutcome::Granted { .. }) => Ok(Granted),
        (Ambiguous, PaymentOutcome::Refused { .. }) => Ok(Refused),
        (Ambiguous, PaymentOutcome::Ambiguous { .. }) => Ok(Ambiguous),
        (Unspent, PaymentOutcome::Granted { .. }) => Ok(Granted),
        (Unspent, PaymentOutcome::Ambiguous { .. }) => Ok(Ambiguous),
        (Unspent, _) => Err(TransitionError {
            reason: "unspent may only start a fresh attempt".into(),
        }),
    }
}
