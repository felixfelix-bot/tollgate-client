//! One purchase attempt, end to end, with the platform seams injected.
//!
//! The order of operations IS the safety property:
//!   1. caps are checked before anything is minted (never mint what you may not spend)
//!   2. the attempt is journalled before the first network call (a duplicate can
//!      never re-post the same note)
//!   3. the token is minted, then posted
//!   4. the answer is classified; an ambiguous answer is reconciled against the
//!      mint, never retried blindly

use crate::policy::{decide, Caps, Decision};
use crate::purchase::{classify_response, intent_key, next_state, IntentState, PaymentOutcome};
use crate::scoring::{GatewayHistory, Offer};
use std::collections::HashMap;

pub trait Wallet {
    /// Mint `amount` of `unit` at `mint_url` and return the encoded token.
    fn mint_token(&mut self, mint_url: &str, unit: &str, amount: u64) -> Result<String, String>;
    /// NUT-07: has this note been spent? The only authority on ambiguity.
    fn is_spent(&mut self, token: &str) -> Result<bool, String>;
}

/// So a runtime-chosen funding source can live in a `Box<dyn Wallet>`.
impl<T: Wallet + ?Sized> Wallet for Box<T> {
    fn mint_token(&mut self, mint_url: &str, unit: &str, amount: u64) -> Result<String, String> {
        (**self).mint_token(mint_url, unit, amount)
    }
    fn is_spent(&mut self, token: &str) -> Result<bool, String> {
        (**self).is_spent(token)
    }
}

pub trait GateTransport {
    /// `POST /` with the raw token. `Err` means "no answer" (timeout, reset),
    /// which is ambiguous, not a refusal.
    fn post_token(&mut self, gate_url: &str, token: &str) -> Result<(u16, String), String>;
}

pub trait Journal {
    fn begin(&mut self, key: &str) -> Result<(), String>;
    fn set_state(&mut self, key: &str, state: IntentState) -> Result<(), String>;
    fn state(&self, key: &str) -> Option<IntentState>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttemptResult {
    NotAttempted {
        reason: String,
    },
    AlreadyAttempted {
        key: String,
        state: IntentState,
    },
    Finished {
        key: String,
        state: IntentState,
        outcome: PaymentOutcome,
        /// What was paid, in the offer's unit.
        cost: f64,
    },
}

pub fn attempt_purchase(
    gateway_id: &str,
    offer: &Offer,
    caps: &Caps,
    attempt_id: &str,
    wallet: &mut dyn Wallet,
    transport: &mut dyn GateTransport,
    journal: &mut dyn Journal,
) -> Result<AttemptResult, String> {
    // 1. Caps first — a refused purchase must cost nothing, not even a mint round trip.
    let mut offer = offer.clone();
    offer.gateway_id = gateway_id.to_string();
    let history: HashMap<String, GatewayHistory> = HashMap::new();
    match decide(std::slice::from_ref(&offer), &history, caps, None)? {
        Decision::Buy { .. } => {}
        Decision::Prompt { reason } | Decision::Refuse { reason } => {
            return Ok(AttemptResult::NotAttempted { reason })
        }
    }

    // 2. Journal before money moves.
    let key = intent_key(attempt_id);
    if let Some(state) = journal.state(&key) {
        return Ok(AttemptResult::AlreadyAttempted { key, state });
    }
    journal.begin(&key)?;

    // 3. Mint.
    let cost = offer.total_cost();
    let amount = cost.round() as u64;
    let token = match wallet.mint_token(&offer.mint_url, &offer.unit, amount) {
        Ok(t) => t,
        Err(e) => {
            journal.set_state(&key, IntentState::Unspent)?;
            return Err(format!(
                "wallet could not mint {amount} {} from {}: {e}",
                offer.unit, offer.mint_url
            ));
        }
    };

    // 4. Post.
    let outcome = match transport.post_token(&offer.gateway_id, &token) {
        Ok((status, body)) => classify_response(status, &body),
        Err(e) => PaymentOutcome::Ambiguous {
            reason: format!("transport error: {e}"),
        },
    };

    // 5. Record, then reconcile an ambiguity against the mint.
    let mut state = next_state(IntentState::Pending, &outcome).map_err(|e| e.to_string())?;
    journal.set_state(&key, state)?;

    if state == IntentState::Ambiguous {
        let spent = wallet.is_spent(&token)?;
        state = next_state(
            IntentState::Ambiguous,
            &PaymentOutcome::Reconciled { spent },
        )
        .map_err(|e| e.to_string())?;
        journal.set_state(&key, state)?;
    }

    Ok(AttemptResult::Finished {
        key,
        state,
        outcome,
        cost,
    })
}
