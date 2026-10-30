//! Offline verified-byte boundary, NOT permission to spend real funds.
//! Snapshot provenance, fee authenticity, network binding and entitlement are
//! external deferred gates. No production transport implementation is supplied.
use crate::{
    budget_store::{BudgetStore, Error, Result},
    send_state::{DeliveryObservation, SendStatus, TrustedSendPlan},
    token_vault::{ProtectedTokenVault, VaultRef},
    validated_token::{FeeKnowledge, SendContext, TrustedMintSnapshot, ValidatedToken},
};

/// Transport receives only the digest-checked, structurally verified vault bytes.
/// Implementations are trusted to send exactly this body, at most once. A malicious
/// implementation can of course perform arbitrary I/O; this is not a sandbox.
/// No closure blanket implementation or production network adapter is provided.
pub trait VerifiedBytesTransport {
    fn send(&mut self, body: &[u8]) -> DeliveryObservation;
}

impl BudgetStore {
    /// Re-derive every bearer claim at the irreversible boundary. Mutable plans
    /// are proposals, not capabilities: mismatches fail before accounting or I/O.
    /// Reserve full decoded face plus the explicitly trusted fee upper bound,
    /// claim canonical mint/Y and commit the irreversible send mark atomically.
    /// Replay is status-only, including after panic, restart or ambiguous delivery.
    /// This dormant API does NOT authenticate snapshots, fees or network identity.
    pub fn send_validated<T: VerifiedBytesTransport>(
        &mut self,
        proposal: &TrustedSendPlan,
        vault: &ProtectedTokenVault,
        snapshot: &TrustedMintSnapshot,
        transport: &mut T,
    ) -> Result<SendStatus> {
        let hex = proposal.vault.as_bytes();
        if hex.len() != 64 {
            return Err(Error::Invalid);
        }
        let mut digest = [0u8; 32];
        for (out, pair) in digest.iter_mut().zip(hex.chunks_exact(2)) {
            let nibble = |b| match b {
                b'0'..=b'9' => Ok(b - b'0'),
                b'a'..=b'f' => Ok(b - b'a' + 10),
                _ => Err(Error::Invalid),
            };
            *out = nibble(pair[0])? * 16 + nibble(pair[1])?;
        }
        let bytes = vault
            .load(&VaultRef::from_digest(digest))
            .map_err(|_| Error::Invalid)?;
        let token = ValidatedToken::parse(&bytes, snapshot).map_err(|_| Error::Invalid)?;
        let derived = token
            .store_send_plan(
                vault,
                SendContext {
                    operation: proposal.operation.clone(),
                    network: proposal.network.clone(),
                    day: proposal.day,
                    cap: proposal.cap,
                },
                FeeKnowledge::TrustedUpperBound(proposal.fee),
            )
            .map_err(|_| Error::Invalid)?;
        // Compare proposals strictly; canonical aliases are accepted by parsing,
        // not by trusting an arbitrary mint label supplied on a mutable plan.
        if &derived != proposal {
            return Err(Error::Invalid);
        }
        self.send_derived(&derived, |_| transport.send(&bytes))
    }
}
