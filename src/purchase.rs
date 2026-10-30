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
        /// The module stamps the MAC it answered for (ADR-0007). A client must
        /// compare it with its own MAC: a mismatch means a proxy/NAT path and the
        /// value is already spent.
        device_identifier: Option<String>,
    },
    /// The gate refused and the note was **not** taken. Terminal.
    Refused { code: String, message: String },
    /// Nothing was consumed; the same note may be sent again after a backoff.
    Retryable { code: String, message: String },
    /// The value is gone (spent, or on a retired keyset). Terminal, and it must
    /// be surfaced to the user rather than silently retried.
    ValueLost { code: String, message: String },
    /// The note may already be spent. Never resend without a NUT-07 answer.
    Ambiguous { reason: String },
    /// The mint answered the ambiguity: the note is spent (grant is owed) or
    /// unspent (safe to retry).
    Reconciled { spent: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Pending,
    Granted,
    Refused,
    Retryable,
    Ambiguous,
    Lost,
    Spent,
    Unspent,
}

impl IntentState {
    pub fn as_str(self) -> &'static str {
        match self {
            IntentState::Pending => "pending",
            IntentState::Granted => "granted",
            IntentState::Refused => "refused",
            IntentState::Retryable => "retryable",
            IntentState::Ambiguous => "ambiguous",
            IntentState::Lost => "lost",
            IntentState::Spent => "spent",
            IntentState::Unspent => "unspent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "pending" => IntentState::Pending,
            "granted" => IntentState::Granted,
            "refused" => IntentState::Refused,
            "retryable" => IntentState::Retryable,
            "ambiguous" => IntentState::Ambiguous,
            "lost" => IntentState::Lost,
            "spent" => IntentState::Spent,
            "unspent" => IntentState::Unspent,
            _ => return None,
        })
    }
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
        let digits: String = stripped
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
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
            device_identifier: string_after(body, "device-identifier"),
        },
        // A notice event is not automatically a refusal: the module's own codes
        // say whether the note survived. See `by_code`.
        (s, Some(21023)) if s >= 500 => PaymentOutcome::Ambiguous {
            reason: format!(
                "gate answered {s} ({}); the note may be spent",
                string_after(body, "code").unwrap_or_else(|| "unknown".into())
            ),
        },
        (_, Some(21023)) => by_code(
            &string_after(body, "code").unwrap_or_else(|| "unknown".to_string()),
            &string_after(body, "message").unwrap_or_default(),
        ),
        _ => PaymentOutcome::Ambiguous {
            reason: format!("unclassifiable answer: HTTP {status}"),
        },
    }
}

/// The module's kind:21023 codes, classified by what they say about the note.
/// Source: docs/client/v1-client-contract.md §5 (every code cited to merchant.go).
///
/// An unknown code is **ambiguous** on purpose: a future code we do not
/// understand must never be read as "safe to resend".
fn by_code(code: &str, message: &str) -> PaymentOutcome {
    let refused = [
        "invalid-mac-address",
        "payment-error-invalid-token",
        "payment-error-below-swap-fee",
        "payment-processing-failed",
        "session-error",
    ];
    let retryable = [
        "device-unresolved",
        "client-not-registered",
        "mint-rate-limited",
        "payment-error-mint-unreachable",
        "service-unavailable",
        "starting",
    ];
    let ambiguous = [
        "payment-duplicate-inflight",
        "payment-received-grant-pending",
        "payment-outcome-unknown",
    ];
    let lost = ["payment-error-token-spent", "payment-error-keyset-expired"];

    let m = message.to_string();
    let c = code.to_string();
    if refused.contains(&code) {
        PaymentOutcome::Refused {
            code: c,
            message: m,
        }
    } else if retryable.contains(&code) {
        PaymentOutcome::Retryable {
            code: c,
            message: m,
        }
    } else if ambiguous.contains(&code) {
        PaymentOutcome::Ambiguous {
            reason: format!("{code}: {message}"),
        }
    } else if lost.contains(&code) {
        PaymentOutcome::ValueLost {
            code: c,
            message: m,
        }
    } else {
        PaymentOutcome::Ambiguous {
            reason: format!("unrecognised code {code}: {message}"),
        }
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
        (Granted, _) | (Spent, _) | (Lost, _) => Ok(current),
        (Pending, PaymentOutcome::Granted { .. }) => Ok(Granted),
        (Pending, PaymentOutcome::Refused { .. }) => Ok(Refused),
        (Pending, PaymentOutcome::Retryable { .. }) => Ok(Retryable),
        (Pending, PaymentOutcome::ValueLost { .. }) => Ok(Lost),
        (Retryable, PaymentOutcome::Granted { .. }) => Ok(Granted),
        (Retryable, PaymentOutcome::Refused { .. }) => Ok(Refused),
        (Retryable, PaymentOutcome::ValueLost { .. }) => Ok(Lost),
        (Retryable, PaymentOutcome::Ambiguous { .. }) => Ok(Ambiguous),
        (Retryable, PaymentOutcome::Retryable { .. }) => Ok(Retryable),
        (Retryable, PaymentOutcome::Reconciled { .. }) => Err(TransitionError {
            reason: "nothing was ambiguous; reconciliation does not apply".into(),
        }),
        (Ambiguous, PaymentOutcome::ValueLost { .. }) => Ok(Lost),
        (Ambiguous, PaymentOutcome::Retryable { .. }) => Ok(Ambiguous),
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
