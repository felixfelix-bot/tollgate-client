//! Dormant send-once seam. Inputs are TRUSTED assertions, NOT Cashu validation.
//! No token parsing, vault, network binding or production transport is provided.
use crate::budget_store::{label, number, BudgetStore, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrustedProofIdentity {
    pub y: String,
    pub keyset: String,
    pub amount: u64,
}
/// Caller must validate canonical mint/Y, keyset/unit, amounts, durable vault content,
/// and stable entitlement/network/generation identity. This type does NOT do so.
#[derive(Clone, Debug)]
pub struct TrustedSendPlan {
    pub operation: String,
    pub network: String,
    pub vault: String,
    pub mint: String,
    pub unit: String,
    pub day: u64,
    pub face: u64,
    pub fee: u64,
    pub cap: u64,
    pub proofs: Vec<TrustedProofIdentity>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum DeliveryObservation {
    Unknown = 0,
    GrantNotice = 1,
    RefusalOrMalformed = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum ProofObservation {
    Unknown = 0,
    AllUnspent = 1,
    PendingOrMixed = 2,
    AllSpent = 3,
}
#[derive(Debug, PartialEq, Eq)]
pub struct SendStatus {
    pub delivery: DeliveryObservation,
    pub proofs: ProofObservation,
    /// This slice has no access verifier; always false, even for all-spent.
    pub internet_verified: bool,
}
fn status(db: &Connection, id: &str) -> Result<SendStatus> {
    let (d, p): (i64, i64) = db.query_row(
        "SELECT delivery,observation FROM sends WHERE operation=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(SendStatus {
        delivery: match d {
            0 => DeliveryObservation::Unknown,
            1 => DeliveryObservation::GrantNotice,
            2 => DeliveryObservation::RefusalOrMalformed,
            _ => return Err(Error::Schema),
        },
        proofs: match p {
            0 => ProofObservation::Unknown,
            1 => ProofObservation::AllUnspent,
            2 => ProofObservation::PendingOrMixed,
            3 => ProofObservation::AllSpent,
            _ => return Err(Error::Schema),
        },
        internet_verified: false,
    })
}
impl BudgetStore {
    /// Only the transaction that INSERTs the irreversible send mark calls transport.
    /// Exact replay is status-only, regardless of attempt ID. No retry capability
    /// escapes this call. Panic/death/error after commit permanently quarantines it.
    pub fn send_once<F>(
        &mut self,
        p: &TrustedSendPlan,
        _caller_attempt: &str,
        transport: F,
    ) -> Result<SendStatus>
    where
        F: FnOnce(&TrustedSendPlan) -> DeliveryObservation,
    {
        for s in [&p.operation, &p.network, &p.vault, &p.mint, &p.unit] {
            label(s)?;
        }
        let mint = crate::mint_identity::canonical(&p.mint).map_err(|_| Error::Invalid)?;
        let mut proofs = p.proofs.clone();
        proofs.sort();
        if proofs.is_empty() {
            return Err(Error::Invalid);
        }
        let mut sum = 0u64;
        let mut seen = std::collections::BTreeSet::new();
        for proof in &proofs {
            label(&proof.y)?;
            label(&proof.keyset)?;
            if proof.amount == 0 || !seen.insert(&proof.y) {
                return Err(Error::Invalid);
            }
            sum = sum.checked_add(proof.amount).ok_or(Error::Invalid)?;
        }
        if sum != p.face {
            return Err(Error::Invalid);
        }
        let total = number(p.face.checked_add(p.fee).ok_or(Error::Invalid)?)?;
        let (day, face, fee, cap) = (
            number(p.day)?,
            number(p.face)?,
            number(p.fee)?,
            number(p.cap)?,
        );
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<(String, String, String)> = tx
            .query_row(
                "SELECT network,vault,mint FROM sends WHERE operation=?1",
                [&p.operation],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        // Never adopt an accounting-only reservation: it might already be terminal
        // or correspond to an external action outside this engine.
        if old.is_none()
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM reservations WHERE id=?1)",
                [&p.operation],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Err(Error::Conflict);
        }
        Self::reserve_on(&tx, &p.operation, &p.unit, day, face, fee, cap, total)?;
        if let Some(old) = old {
            let existing=tx.prepare("SELECT y,keyset,amount FROM proof_claims WHERE operation=?1 ORDER BY y,keyset,amount")?.query_map([&p.operation],|r|Ok(TrustedProofIdentity {y:r.get(0)?,keyset:r.get(1)?,amount:r.get(2)?}))?.collect::<std::result::Result<Vec<_>,_>>()?;
            if old != (p.network.clone(), p.vault.clone(), mint.clone()) || existing != proofs {
                return Err(Error::Conflict);
            }
            return status(&tx, &p.operation);
        }
        tx.execute(
            "INSERT INTO sends VALUES(?1,?2,?3,?4,'post_may_have_started',0,0)",
            params![p.operation, p.network, p.vault, mint],
        )?;
        for proof in proofs {
            tx.execute(
                "INSERT INTO proof_claims VALUES(?1,?2,?3,?4,?5)",
                params![
                    mint,
                    proof.y,
                    proof.keyset,
                    number(proof.amount)?,
                    p.operation
                ],
            )?;
        }
        tx.commit()?;
        // No DB transaction spans network I/O. Permanent unique ownership means
        // another process can only read status, never acquire a send lease.
        let delivery = transport(p);
        self.db.execute(
            "UPDATE sends SET delivery=?2 WHERE operation=?1",
            params![p.operation, delivery as i64],
        )?;
        status(&self.db, &p.operation)
    }
    pub fn send_status(&self, operation: &str) -> Result<SendStatus> {
        status(&self.db, operation)
    }
    /// Trusted diagnostic snapshot only. Never releases exposure or authorizes POST.
    /// All-spent stays sticky; neither it nor a grant notice establishes access.
    pub fn observe_proofs(&mut self, operation: &str, observation: ProofObservation) -> Result<()> {
        let changed=self.db.execute("UPDATE sends SET observation=CASE WHEN observation=3 THEN 3 ELSE ?2 END WHERE operation=?1",params![operation,observation as i64])?;
        if changed != 1 {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
