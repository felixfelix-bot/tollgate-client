//! Offline simulation adapter. Enrollment is local trust, not issuer authentication.
//! No production transport, clock authentication, or same-UID rollback protection.
use crate::{
    budget_store::BudgetStore,
    token_vault::{ProtectedTokenVault, VaultRef},
    validated_send::VerifiedBytesTransport,
    validated_token::{FeeKnowledge, SendContext, TrustedMintSnapshot, ValidatedToken},
};
use cdk::{
    mint_url::MintUrl,
    nuts::{CurrencyUnit, KeySet},
    secp256k1::hashes::{sha256, Hash},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Enrollment {
    #[serde(deserialize_with = "unique_caps")]
    pub caps: BTreeMap<String, u64>,
    pub fee: u64,
    pub snapshot_sha256: String,
    pub valid_from: u64,
    pub valid_until: u64,
}

fn unique_caps<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, u64>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique integer caps")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut m: M,
        ) -> Result<Self::Value, M::Error> {
            let mut caps = BTreeMap::new();
            while let Some((k, v)) = m.next_entry::<String, u64>()? {
                if caps.insert(k, v).is_some() {
                    return Err(serde::de::Error::custom("duplicate cap key"));
                }
            }
            Ok(caps)
        }
    }
    d.deserialize_map(Unique)
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BoundPolicy {
    identity: String,
    policy: Enrollment,
}

pub struct PolicyAdapter {
    ledger: BudgetStore,
    policy: Enrollment,
    path: PathBuf,
    identity: String,
}
fn policy_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(".policy.json");
    PathBuf::from(name)
}
fn validate(policy: &Enrollment) -> Result<(), String> {
    if policy.caps.is_empty()
        || policy.valid_from >= policy.valid_until
        || policy.snapshot_sha256.len() != 64
        || !policy
            .snapshot_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid enrollment".into());
    }
    crate::budget_store::number(policy.fee).map_err(|_| "invalid fee")?;
    for (unit, cap) in &policy.caps {
        let parsed = CurrencyUnit::from_str(unit).map_err(|_| "invalid unit")?;
        if parsed.to_string() != *unit {
            return Err("noncanonical unit".into());
        }
        crate::budget_store::number(*cap).map_err(|_| "invalid cap")?;
    }
    Ok(())
}
fn private_parent(path: &Path) -> Result<File, String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::metadata(parent).map_err(|_| "missing policy directory")?;
    // Reuse the vault's ancestor ownership, symlink and private-directory checks.
    ProtectedTokenVault::open(parent).map_err(|_| "unsafe policy directory")?;
    File::open(parent).map_err(|_| "policy directory unavailable".into())
}
fn load_policy(path: &Path) -> Result<BoundPolicy, String> {
    let parent = private_parent(path)?;
    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
    let nofollow = 0o100000;
    #[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
    let nofollow = 0o400000;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nofollow | 0o4000)
        .open(path)
        .map_err(|_| "policy missing or unreadable")?;
    let meta = file.metadata().map_err(|_| "policy metadata unavailable")?;
    if !meta.is_file()
        || meta.mode() & 0o7777 != 0o600
        || meta.nlink() != 1
        || meta.uid()
            != parent
                .metadata()
                .map_err(|_| "policy owner unavailable")?
                .uid()
        || meta.len() > 1024 * 1024
    {
        return Err("unsafe policy file".into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| "policy read failed")?;
    let policy: BoundPolicy = serde_json::from_slice(&bytes).map_err(|_| "invalid policy file")?;
    validate(&policy.policy)?;
    Ok(policy)
}
impl PolicyAdapter {
    /// Exclusive enrollment: failures may leave unusable partial state, never reset it.
    /// Both files require a private local durable directory. Creation of the ledger
    /// is the exclusive claim; the adapter is usable only after policy fsync succeeds.
    pub fn enroll(db_path: &Path, policy: &Enrollment) -> Result<Self, String> {
        validate(policy)?;
        let path = policy_path(db_path);
        let parent = private_parent(&path)?;
        if std::fs::symlink_metadata(&path).is_ok() {
            return Err("already enrolled".into());
        }
        let mut ledger = BudgetStore::create(db_path).map_err(|_| "ledger enrollment failed")?;
        let location =
            std::fs::canonicalize(db_path).map_err(|_| "installation path unavailable")?;
        let location = location.to_str().ok_or("non UTF-8 installation path")?;
        let tx = ledger
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| "enrollment transaction failed")?;
        let identity: String = tx
            .query_row("SELECT lower(hex(randomblob(32)))", [], |r| r.get(0))
            .map_err(|_| "identity generation failed")?;
        let encoded = serde_json::to_string(policy).map_err(|_| "policy encoding failed")?;
        tx.execute("UPDATE metadata SET policy_identity=?1, policy_json=?2, installation_path=?3 WHERE singleton=1 AND policy_identity IS NULL",
            rusqlite::params![identity, encoded, location]).map_err(|_| "binding failed")?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| "policy enrollment failed")?;
        let bytes = serde_json::to_vec(&BoundPolicy {
            identity: identity.clone(),
            policy: policy.clone(),
        })
        .map_err(|_| "policy encoding failed")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "policy persistence failed")?;
        parent
            .sync_all()
            .map_err(|_| "policy directory sync failed")?;
        tx.commit().map_err(|_| "binding commit failed")?;
        Ok(Self {
            ledger,
            policy: policy.clone(),
            path,
            identity,
        })
    }
    pub fn open(db_path: &Path) -> Result<Self, String> {
        let path = policy_path(db_path);
        let bound = load_policy(&path)?;
        let ledger = BudgetStore::open(db_path).map_err(|_| "ledger open failed")?;
        let stored: (String, String, String) = ledger.db.query_row(
            "SELECT policy_identity, policy_json, installation_path FROM metadata WHERE singleton=1", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|_| "incomplete enrollment")?;
        let location =
            std::fs::canonicalize(db_path).map_err(|_| "installation path unavailable")?;
        if stored.0 != bound.identity
            || stored.1
                != serde_json::to_string(&bound.policy).map_err(|_| "policy encoding failed")?
            || Some(stored.2.as_str()) != location.to_str()
        {
            return Err("policy/ledger binding mismatch".into());
        }
        Ok(Self {
            ledger,
            policy: bound.policy,
            path,
            identity: bound.identity,
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn attempt(
        &mut self,
        expected: &Enrollment,
        snapshot_bytes: &[u8],
        vault: &ProtectedTokenVault,
        reference: &VaultRef,
        operation: &str,
        day: u64,
        now: u64,
        transport: &mut impl VerifiedBytesTransport,
    ) -> Result<u64, String> {
        let bound = load_policy(&self.path)?;
        if expected != &self.policy
            || bound.policy != self.policy
            || bound.identity != self.identity
        {
            return Err("enrollment mismatch".into());
        }
        if now < self.policy.valid_from || now >= self.policy.valid_until {
            return Err("snapshot outside freshness interval".into());
        }
        if sha256::Hash::hash(snapshot_bytes).to_string() != self.policy.snapshot_sha256 {
            return Err("snapshot digest mismatch".into());
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Snapshot {
            mint: String,
            unit: CurrencyUnit,
            keysets: Vec<KeySet>,
        }
        let raw: Snapshot =
            serde_json::from_slice(snapshot_bytes).map_err(|_| "invalid snapshot JSON")?;
        let unit = raw.unit.to_string();
        let snapshot = TrustedMintSnapshot::new(
            MintUrl::from_str(&raw.mint).map_err(|_| "invalid mint")?,
            raw.unit,
            raw.keysets,
        )
        .map_err(|_| "invalid snapshot")?;
        let bytes = vault.load(reference).map_err(|_| "vault load failed")?;
        let token =
            ValidatedToken::parse(&bytes, &snapshot).map_err(|_| "token validation failed")?;
        // Parsing proved token unit equals snapshot unit; never accept a caller unit.
        let cap = *self.policy.caps.get(&unit).ok_or("unit not enrolled")?;
        let plan = token
            .store_send_plan(
                vault,
                SendContext {
                    operation: operation.into(),
                    network: "offline-simulation".into(),
                    day,
                    cap,
                },
                FeeKnowledge::TrustedUpperBound(self.policy.fee),
            )
            .map_err(|_| "plan validation failed")?;
        self.ledger
            .send_validated(&plan, vault, &snapshot, transport)
            .map_err(|_| "validated send refused")?;
        self.ledger
            .usage(&unit, day)
            .map_err(|_| "usage unavailable".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enrollment_is_immutable_and_missing_policy_fails_closed() {
        let dir =
            std::path::PathBuf::from(format!("/dev/shm/offline-policy-{}", std::process::id()));
        let vault = crate::token_vault::ProtectedTokenVault::open(&dir).unwrap();
        let db = dir.join("ledger");
        let policy = Enrollment {
            caps: [("sat".into(), 10)].into(),
            fee: 1,
            snapshot_sha256: "ab".repeat(32),
            valid_from: 10,
            valid_until: 20,
        };
        let adapter = PolicyAdapter::enroll(&db, &policy).unwrap();
        drop(adapter);
        assert!(PolicyAdapter::enroll(&db, &policy).is_err());
        let adapter = PolicyAdapter::open(&db).unwrap();
        assert_eq!(adapter.policy, policy);
        drop(adapter);
        std::fs::remove_file(policy_path(&db)).unwrap();
        assert!(PolicyAdapter::open(&db).is_err());
        assert!(PolicyAdapter::enroll(&db, &policy).is_err());
        drop(vault);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
