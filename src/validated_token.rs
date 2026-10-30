//! Offline structural validation only; never establishes issuer authenticity or unspentness.
use cdk::mint_url::MintUrl;
use cdk::nuts::{CurrencyUnit, KeySet, KeySetInfo, Token};
use std::{collections::BTreeSet, str::FromStr};

#[derive(Debug, PartialEq, Eq)]
pub enum ValidationError {
    Encoding,
    Mint,
    Unit,
    Metadata,
    Empty,
    Denomination,
    Duplicate,
    Overflow,
    UnknownFee,
    Vault,
}
/// Explicit trust boundary: caller authenticates the mint-to-keyset association
/// out of band. ID verification checks internal consistency, NOT provenance or
/// freshness. Inactive keysets are valid inputs; no issuance is performed here.
pub struct TrustedMintSnapshot {
    mint: MintUrl,
    unit: CurrencyUnit,
    keysets: Vec<KeySet>,
}
impl TrustedMintSnapshot {
    pub fn new(
        mint: MintUrl,
        unit: CurrencyUnit,
        keysets: Vec<KeySet>,
    ) -> Result<Self, ValidationError> {
        let mint = MintUrl::from_str(
            &crate::mint_identity::canonical(&mint.to_string())
                .map_err(|_| ValidationError::Mint)?,
        )
        .map_err(|_| ValidationError::Mint)?;
        let mut ids = BTreeSet::new();
        if keysets.is_empty() {
            return Err(ValidationError::Metadata);
        }
        for keyset in &keysets {
            if keyset.unit != unit
                || !ids.insert(keyset.id)
                || keyset.keys.is_empty()
                || keyset.verify_id().is_err()
            {
                return Err(ValidationError::Metadata);
            }
        }
        Ok(Self {
            mint,
            unit,
            keysets,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProofIdentity {
    pub y: String,
    pub keyset: String,
    pub amount: u64,
}
/// Bearer bytes remain private and this type intentionally implements neither
/// Debug nor Display. Validation is structural only: signatures/DLEQ, NUT-07
/// state, and spending conditions are NOT verified. No memory zeroization claim.
pub struct ValidatedToken {
    bytes: Vec<u8>,
    mint: MintUrl,
    unit: CurrencyUnit,
    face: u64,
    proofs: Vec<ProofIdentity>,
}
impl ValidatedToken {
    pub fn parse(bytes: &[u8], snapshot: &TrustedMintSnapshot) -> Result<Self, ValidationError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ValidationError::Encoding)?;
        let token = Token::from_str(text).map_err(|_| ValidationError::Encoding)?;
        let mint = token.mint_url().map_err(|_| ValidationError::Mint)?;
        let mint = MintUrl::from_str(
            &crate::mint_identity::canonical(&mint.to_string())
                .map_err(|_| ValidationError::Mint)?,
        )
        .map_err(|_| ValidationError::Mint)?;
        if mint != snapshot.mint {
            return Err(ValidationError::Mint);
        }
        let unit = token.unit().ok_or(ValidationError::Unit)?;
        if unit != snapshot.unit {
            return Err(ValidationError::Unit);
        }
        let infos: Vec<_> = snapshot
            .keysets
            .iter()
            .map(|k| KeySetInfo {
                id: k.id,
                unit: k.unit.clone(),
                active: k.active.unwrap_or(false),
                input_fee_ppk: k.input_fee_ppk,
                final_expiry: k.final_expiry,
            })
            .collect();
        let raw = token
            .proofs(&infos)
            .map_err(|_| ValidationError::Metadata)?;
        if raw.is_empty() {
            return Err(ValidationError::Empty);
        }
        let mut proofs = Vec::new();
        let mut ys = BTreeSet::new();
        let mut face = 0u64;
        for p in raw {
            let k = snapshot
                .keysets
                .iter()
                .find(|k| k.id == p.keyset_id)
                .ok_or(ValidationError::Metadata)?;
            let amount = u64::from(p.amount);
            if amount == 0 || k.keys.amount_key(p.amount).is_none() {
                return Err(ValidationError::Denomination);
            }
            let y = p.y().map_err(|_| ValidationError::Encoding)?.to_string();
            if !ys.insert(y.clone()) {
                return Err(ValidationError::Duplicate);
            }
            face = face.checked_add(amount).ok_or(ValidationError::Overflow)?;
            proofs.push(ProofIdentity {
                y,
                keyset: p.keyset_id.to_string(),
                amount,
            });
        }
        proofs.sort();
        Ok(Self {
            bytes: bytes.to_vec(),
            mint,
            unit,
            face,
            proofs,
        })
    }
    pub fn face(&self) -> u64 {
        self.face
    }
    pub fn proof_identities(&self) -> &[ProofIdentity] {
        &self.proofs
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        self.mint == other.mint
            && self
                .proofs
                .iter()
                .any(|a| other.proofs.iter().any(|b| a.y == b.y))
    }
    pub fn checked_total(&self, fee: FeeKnowledge) -> Result<u64, ValidationError> {
        match fee {
            FeeKnowledge::Unknown => Err(ValidationError::UnknownFee),
            FeeKnowledge::TrustedUpperBound(fee) => {
                self.face.checked_add(fee).ok_or(ValidationError::Overflow)
            }
        }
    }
}
#[derive(Clone, Copy)]
pub enum FeeKnowledge {
    Unknown,
    TrustedUpperBound(u64),
}

#[cfg(all(
    feature = "persistence",
    any(target_os = "linux", target_os = "android")
))]
impl ValidatedToken {
    /// Persist structurally validated bytes before producing the dormant accounting
    /// plan. This does NOT authorize payment or establish mint authenticity, spend
    /// state, spending-condition satisfaction, or entitlement/network binding.
    pub fn store_send_plan(
        &self,
        vault: &crate::token_vault::ProtectedTokenVault,
        context: SendContext,
        fee: FeeKnowledge,
    ) -> Result<crate::send_state::TrustedSendPlan, ValidationError> {
        let total = self.checked_total(fee)?;
        let fee = match fee {
            FeeKnowledge::TrustedUpperBound(n) => n,
            FeeKnowledge::Unknown => return Err(ValidationError::UnknownFee),
        };
        for n in [total, context.day, context.cap] {
            crate::budget_store::number(n).map_err(|_| ValidationError::Overflow)?;
        }
        for s in [&context.operation, &context.network] {
            crate::budget_store::label(s).map_err(|_| ValidationError::Metadata)?;
        }
        let reference = vault
            .store(&self.bytes)
            .map_err(|_| ValidationError::Vault)?;
        Ok(crate::send_state::TrustedSendPlan {
            operation: context.operation,
            network: context.network,
            day: context.day,
            cap: context.cap,
            vault: reference
                .digest()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            mint: self.mint.to_string(),
            unit: self.unit.to_string(),
            face: self.face,
            fee,
            proofs: self
                .proofs
                .iter()
                .map(|p| crate::send_state::TrustedProofIdentity {
                    y: p.y.clone(),
                    keyset: p.keyset.clone(),
                    amount: p.amount,
                })
                .collect(),
        })
    }
}
/// Entitlement/network identity and budget policy remain caller responsibilities.
pub struct SendContext {
    pub operation: String,
    pub network: String,
    pub day: u64,
    pub cap: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdk::{
        dhke::{hash_to_curve, sign_message},
        nuts::{nut00::token::TokenV3Token, Id, Keys, Proof, SecretKey, Token, TokenV3},
        secret::Secret,
        Amount,
    };
    use std::{collections::BTreeMap, str::FromStr};
    #[cfg(all(
        feature = "persistence",
        any(target_os = "linux", target_os = "android")
    ))]
    #[test]
    fn typed_adapter_stores_exact_bytes_and_uses_face_not_requested_amount() {
        use crate::token_vault::ProtectedTokenVault;
        let path =
            std::path::PathBuf::from(format!("/dev/shm/validated-adapter-{}", std::process::id()));
        let vault = ProtectedTokenVault::open(&path).unwrap();
        let (s, sk) = fixture();
        let t = token(&s, vec![proof(&s, &sk, "adapter", 2)]);
        let bytes = t.to_string();
        let v = ValidatedToken::parse(bytes.as_bytes(), &s).unwrap();
        let ctx = || SendContext {
            operation: "op".into(),
            network: "network".into(),
            day: 1,
            cap: 10,
        };
        assert!(matches!(
            v.store_send_plan(&vault, ctx(), FeeKnowledge::Unknown),
            Err(ValidationError::UnknownFee)
        ));
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        let plan = v
            .store_send_plan(&vault, ctx(), FeeKnowledge::TrustedUpperBound(1))
            .unwrap();
        assert_eq!(plan.face, 2);
        assert_eq!(plan.fee, 1);
        assert_eq!(plan.unit, "sat");
        assert_eq!(plan.proofs[0].y, v.proof_identities()[0].y);
        let reference = vault.store(bytes.as_bytes()).unwrap();
        assert_eq!(
            plan.vault,
            reference
                .digest()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert!(vault.load(&reference).unwrap() == bytes.as_bytes());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[cfg(all(feature = "persistence", target_os = "linux"))]
    #[test]
    fn mint_alias_unmodified_plans_send_only_once() {
        use crate::{
            budget_store::BudgetStore, send_state::DeliveryObservation,
            token_vault::ProtectedTokenVault,
        };
        let path = std::path::PathBuf::from(format!("/dev/shm/mint-alias-{}", std::process::id()));
        let vault = ProtectedTokenVault::open(&path).unwrap();
        let (s, sk) = fixture();
        let alias = TrustedMintSnapshot::new(
            MintUrl::from_str("https://MINT.example:443/").unwrap(),
            s.unit.clone(),
            s.keysets.clone(),
        )
        .unwrap();
        let proof = proof(&s, &sk, "same-bearer", 2);
        let a = parse(&token(&s, vec![proof.clone()]), &s).unwrap();
        let b = parse(&token(&alias, vec![proof]), &alias).unwrap();
        assert_eq!(a.proof_identities(), b.proof_identities());
        let ctx = |id: &str| SendContext {
            operation: id.into(),
            network: "network".into(),
            day: 1,
            cap: 10,
        };
        let first = a
            .store_send_plan(&vault, ctx("first"), FeeKnowledge::TrustedUpperBound(0))
            .unwrap();
        let second = b
            .store_send_plan(&vault, ctx("second"), FeeKnowledge::TrustedUpperBound(0))
            .unwrap();
        let mut db = BudgetStore::create(path.join("ledger")).unwrap();
        let mut sends = 0;
        db.send_once_nonproduction(&first, "one", |_| {
            sends += 1;
            DeliveryObservation::Unknown
        })
        .unwrap();
        let second_result = db.send_once_nonproduction(&second, "two", |_| {
            sends += 1;
            DeliveryObservation::Unknown
        });
        std::fs::remove_dir_all(path).unwrap();
        assert_eq!(
            sends, 1,
            "two unmodified typed plans must not send the same bearer twice"
        );
        assert!(second_result.is_err());
        assert!(a.overlaps(&b));
    }
    #[cfg(all(feature = "persistence", target_os = "linux"))]
    #[test]
    fn mint_identity_matrix_typed_plans_and_restart() {
        use crate::{
            budget_store::BudgetStore, send_state::DeliveryObservation,
            token_vault::ProtectedTokenVault,
        };
        let cases = [
            ("https://MINT.example:443/", true),
            ("HTTPS://mint.example/", true),
            ("https://mint.example:444", false),
            ("http://mint.example:80", false),
            ("https://other.example", false),
            ("https://mint.example/mint", false),
            ("https://mint.example/Mint", false),
        ];
        for (i, (url, equivalent)) in cases.into_iter().enumerate() {
            let path = std::path::PathBuf::from(format!(
                "/dev/shm/mint-matrix-{}-{i}",
                std::process::id()
            ));
            let vault = ProtectedTokenVault::open(&path).unwrap();
            let (s, sk) = fixture();
            let other = TrustedMintSnapshot::new(
                MintUrl::from_str(url).unwrap(),
                s.unit.clone(),
                s.keysets.clone(),
            )
            .unwrap();
            let p = proof(&s, &sk, "same", 1);
            let a = parse(&token(&s, vec![p.clone()]), &s).unwrap();
            // Construct token using original spelling, not normalized snapshot field.
            let t = Token::new(
                MintUrl::from_str(url).unwrap(),
                vec![p],
                None,
                CurrencyUnit::Sat,
            );
            let b = parse(&t, &other).unwrap();
            assert_eq!(parse(&t, &s).is_ok(), equivalent);
            assert_eq!(a.overlaps(&b), equivalent);
            let ctx = |id: &str| SendContext {
                operation: id.into(),
                network: "net".into(),
                day: 1,
                cap: 10,
            };
            let first = a
                .store_send_plan(&vault, ctx("a"), FeeKnowledge::TrustedUpperBound(0))
                .unwrap();
            let second = b
                .store_send_plan(&vault, ctx("b"), FeeKnowledge::TrustedUpperBound(0))
                .unwrap();
            let ledger = path.join("db");
            let mut db = BudgetStore::create(&ledger).unwrap();
            let mut calls = 0;
            db.send_once_nonproduction(&first, "1", |_| {
                calls += 1;
                DeliveryObservation::Unknown
            })
            .unwrap();
            drop(db);
            let mut db = BudgetStore::open(&ledger).unwrap();
            let result = db.send_once_nonproduction(&second, "2", |_| {
                calls += 1;
                DeliveryObservation::Unknown
            });
            assert_eq!(result.is_err(), equivalent, "{url}");
            assert_eq!(calls, if equivalent { 1 } else { 2 });
            drop(db);
            // Old identity schemes must never be silently reopened, even with claims.
            let old = rusqlite::Connection::open(&ledger).unwrap();
            old.execute(
                "UPDATE sends SET mint='https://mint.example:443' WHERE operation='a'",
                [],
            )
            .unwrap();
            old.execute(
                "UPDATE proof_claims SET mint='https://mint.example:443' WHERE operation='a'",
                [],
            )
            .unwrap();
            assert!(matches!(
                BudgetStore::open(&ledger),
                Err(crate::budget_store::Error::Schema)
            ));
            old.pragma_update(None, "user_version", 2).unwrap();
            drop(old);
            assert!(matches!(
                BudgetStore::open(&ledger),
                Err(crate::budget_store::Error::Schema)
            ));
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[cfg(all(feature = "persistence", target_os = "linux"))]
    #[test]
    fn opaque_boundary_rejects_mutable_claims_and_substitution() {
        use crate::{budget_store::BudgetStore, token_vault::ProtectedTokenVault};
        let mut bypasses = Vec::new();
        for attack in 0..5 {
            let path = std::path::PathBuf::from(format!(
                "/dev/shm/opaque-red-{}-{attack}",
                std::process::id()
            ));
            let vault = ProtectedTokenVault::open(&path).unwrap();
            let (s, sk) = fixture();
            let bytes = token(&s, vec![proof(&s, &sk, "opaque", 2)]).to_string();
            let v = ValidatedToken::parse(bytes.as_bytes(), &s).unwrap();
            let mut plan = v
                .store_send_plan(
                    &vault,
                    SendContext {
                        operation: "op".into(),
                        network: "net".into(),
                        day: 1,
                        cap: 10,
                    },
                    FeeKnowledge::TrustedUpperBound(0),
                )
                .unwrap();
            match attack {
                0 => {
                    plan.face = 1;
                    plan.proofs[0].amount = 1;
                }
                1 => plan.proofs[0].y = "forged-y".into(),
                2 => plan.vault = "00".repeat(32),
                3 => plan.mint = "https://other.example".into(),
                _ => (),
            }
            let mut db = BudgetStore::create(path.join("db")).unwrap();
            let mut transport = RecordingTransport::default();
            let result = db.send_validated(&plan, &vault, &s, &mut transport);
            let delivered = transport.bodies;
            drop(db);
            std::fs::remove_dir_all(path).unwrap();
            if (attack < 4 && result.is_ok())
                || (attack == 4 && delivered != vec![bytes.as_bytes().to_vec()])
            {
                bypasses.push(attack);
            }
        }
        assert!(bypasses.is_empty(), "accepted bypasses: {bypasses:?}");
    }
    #[cfg(all(feature = "persistence", target_os = "linux"))]
    #[derive(Default)]
    struct RecordingTransport {
        bodies: Vec<Vec<u8>>,
    }
    #[cfg(all(feature = "persistence", target_os = "linux"))]
    impl crate::validated_send::VerifiedBytesTransport for RecordingTransport {
        fn send(&mut self, body: &[u8]) -> crate::send_state::DeliveryObservation {
            self.bodies.push(body.to_vec());
            crate::send_state::DeliveryObservation::Unknown
        }
    }
    #[cfg(all(feature = "persistence", target_os = "linux"))]
    #[test]
    fn verified_send_faults_replays_aliases_and_full_debit() {
        use crate::{budget_store::BudgetStore, token_vault::ProtectedTokenVault};
        for fault in 0..10 {
            let path = std::path::PathBuf::from(format!(
                "/dev/shm/opaque-fault-{}-{fault}",
                std::process::id()
            ));
            let vault = ProtectedTokenVault::open(&path).unwrap();
            let (s, sk) = fixture();
            let t = token(&s, vec![proof(&s, &sk, "same", 2)]);
            let bytes = t.to_string();
            let v = parse(&t, &s).unwrap();
            let context = || SendContext {
                operation: "op".into(),
                network: "net".into(),
                day: 1,
                cap: 3,
            };
            let mut plan = v
                .store_send_plan(&vault, context(), FeeKnowledge::TrustedUpperBound(1))
                .unwrap();
            let file = path.join(&plan.vault);
            let mut db = BudgetStore::create(path.join("db")).unwrap();
            let mut transport = RecordingTransport::default();
            match fault {
                0 => std::fs::remove_file(&file).unwrap(),
                1 => std::fs::write(&file, b"corrupt").unwrap(),
                2 => plan.vault = "gg".repeat(32),
                3 => plan.vault = "short".into(),
                4 => plan.cap = 2,
                5 => {
                    let other = token(&s, vec![proof(&s, &sk, "different", 1)]).to_string();
                    let r = vault.store(other.as_bytes()).unwrap();
                    plan.vault = r.digest().iter().map(|b| format!("{b:02x}")).collect();
                }
                6 => {
                    let r = vault.store(b"invalid-token").unwrap();
                    plan.vault = r.digest().iter().map(|b| format!("{b:02x}")).collect();
                }
                7 => plan.unit = "msat".into(),
                8 => plan.fee = u64::MAX,
                _ => (),
            }
            let result = db.send_validated(&plan, &vault, &s, &mut transport);
            if fault < 9 {
                assert!(result.is_err(), "fault {fault}");
                assert!(transport.bodies.is_empty());
                assert!(db.send_status("op").is_err());
            } else {
                result.unwrap();
                assert_eq!(transport.bodies, vec![bytes.as_bytes().to_vec()]);
                drop(db);
                db = BudgetStore::open(path.join("db")).unwrap();
                db.send_validated(&plan, &vault, &s, &mut transport)
                    .unwrap();
                // A different vault reference for the same proofs must not resend.
                let v3 = ValidatedToken::parse(t.to_v3_string().as_bytes(), &s).unwrap();
                let mut second = v3
                    .store_send_plan(&vault, context(), FeeKnowledge::TrustedUpperBound(1))
                    .unwrap();
                assert!(db
                    .send_validated(&second, &vault, &s, &mut transport)
                    .is_err());
                second.operation = "new-operation".into();
                second.cap = 100;
                assert!(db
                    .send_validated(&second, &vault, &s, &mut transport)
                    .is_err());
                let alias = TrustedMintSnapshot::new(
                    MintUrl::from_str("https://MINT.example:443/").unwrap(),
                    s.unit.clone(),
                    s.keysets.clone(),
                )
                .unwrap();
                db.send_validated(&plan, &vault, &alias, &mut transport)
                    .unwrap();
                assert_eq!(transport.bodies.len(), 1);
                let sql = rusqlite::Connection::open(path.join("db")).unwrap();
                let debit: i64 = sql
                    .query_row("SELECT face+fee FROM reservations WHERE id='op'", [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert_eq!(debit, 3);
            }
            drop(db);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[cfg(all(feature = "persistence", target_os = "linux"))]
    #[test]
    fn verified_transport_panic_is_quarantined_on_restart() {
        use crate::{budget_store::BudgetStore, token_vault::ProtectedTokenVault};
        struct Interrupted;
        impl crate::validated_send::VerifiedBytesTransport for Interrupted {
            fn send(&mut self, _: &[u8]) -> crate::send_state::DeliveryObservation {
                panic!("interrupted transport")
            }
        }
        let path =
            std::path::PathBuf::from(format!("/dev/shm/opaque-panic-{}", std::process::id()));
        let vault = ProtectedTokenVault::open(&path).unwrap();
        let (s, sk) = fixture();
        let v = parse(&token(&s, vec![proof(&s, &sk, "panic", 2)]), &s).unwrap();
        let plan = v
            .store_send_plan(
                &vault,
                SendContext {
                    operation: "op".into(),
                    network: "net".into(),
                    day: 1,
                    cap: 10,
                },
                FeeKnowledge::TrustedUpperBound(0),
            )
            .unwrap();
        let mut db = BudgetStore::create(path.join("db")).unwrap();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| db.send_validated(
                &plan,
                &vault,
                &s,
                &mut Interrupted
            )))
            .is_err()
        );
        drop(db);
        let mut db = BudgetStore::open(path.join("db")).unwrap();
        let mut transport = RecordingTransport::default();
        db.send_validated(&plan, &vault, &s, &mut transport)
            .unwrap();
        let mut renamed = plan.clone();
        renamed.operation = "other".into();
        assert!(db
            .send_validated(&renamed, &vault, &s, &mut transport)
            .is_err());
        assert!(transport.bodies.is_empty());
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
    fn fixture() -> (TrustedMintSnapshot, SecretKey) {
        let sk = SecretKey::from_str(&"01".repeat(32)).unwrap();
        let keys = Keys::new(
            [1u64, 2, 1 << 63]
                .into_iter()
                .map(|n| (Amount::from(n), sk.public_key()))
                .collect::<BTreeMap<_, _>>(),
        );
        let ks = KeySet {
            id: Id::v1_from_keys(&keys),
            unit: CurrencyUnit::Sat,
            active: Some(false),
            keys,
            input_fee_ppk: 0,
            final_expiry: None,
        };
        (
            TrustedMintSnapshot::new(
                MintUrl::from_str("https://mint.example").unwrap(),
                CurrencyUnit::Sat,
                vec![ks],
            )
            .unwrap(),
            sk,
        )
    }
    fn proof(s: &TrustedMintSnapshot, sk: &SecretKey, text: &str, amount: u64) -> Proof {
        let secret = Secret::from_str(text).unwrap();
        let c = sign_message(sk, &hash_to_curve(secret.as_bytes()).unwrap()).unwrap();
        Proof::new(amount.into(), s.keysets[0].id, secret, c)
    }
    fn token(s: &TrustedMintSnapshot, proofs: Vec<Proof>) -> Token {
        Token::new(s.mint.clone(), proofs, None, s.unit.clone())
    }
    fn parse(t: &Token, s: &TrustedMintSnapshot) -> Result<ValidatedToken, ValidationError> {
        ValidatedToken::parse(t.to_string().as_bytes(), s)
    }
    fn is_error(r: Result<ValidatedToken, ValidationError>, e: ValidationError) {
        assert!(matches!(r, Err(actual) if actual == e));
    }
    #[test]
    fn encoding_order_and_overlap_use_y() {
        let (s, sk) = fixture();
        let a = proof(&s, &sk, "synthetic-a", 1);
        let b = proof(&s, &sk, "synthetic-b", 2);
        let t = token(&s, vec![a.clone(), b.clone()]);
        let v = parse(&t, &s).unwrap();
        let v3 = ValidatedToken::parse(t.to_v3_string().as_bytes(), &s).unwrap();
        let reversed = parse(&token(&s, vec![b, a.clone()]), &s).unwrap();
        assert_eq!(v.face(), 3);
        assert_eq!(v.proof_identities(), v3.proof_identities());
        assert_eq!(v.proof_identities(), reversed.proof_identities());
        assert!(v.overlaps(&parse(&token(&s, vec![a]), &s).unwrap()));
        assert!(!v.overlaps(&parse(&token(&s, vec![proof(&s, &sk, "new", 1)]), &s).unwrap()));
    }
    #[test]
    fn duplicates_rejected_even_if_amount_and_signature_differ() {
        let (s, sk) = fixture();
        let a = proof(&s, &sk, "same", 1);
        let mut b = proof(&s, &sk, "same", 2);
        b.c = sk.public_key();
        is_error(
            parse(&token(&s, vec![a, b]), &s),
            ValidationError::Duplicate,
        );
    }
    #[test]
    fn wrong_mint_unit_and_multiple_mints_rejected() {
        let (s, sk) = fixture();
        let p = proof(&s, &sk, "a", 1);
        let other = MintUrl::from_str("https://other.example").unwrap();
        is_error(
            parse(
                &Token::new(other.clone(), vec![p.clone()], None, CurrencyUnit::Sat),
                &s,
            ),
            ValidationError::Mint,
        );
        is_error(
            parse(
                &Token::new(s.mint.clone(), vec![p.clone()], None, CurrencyUnit::Msat),
                &s,
            ),
            ValidationError::Unit,
        );
        let v3 = Token::TokenV3(TokenV3 {
            token: vec![
                TokenV3Token::new(s.mint.clone(), vec![p.clone()]),
                TokenV3Token::new(other, vec![p]),
            ],
            memo: None,
            unit: Some(CurrencyUnit::Sat),
        });
        is_error(parse(&v3, &s), ValidationError::Mint);
    }
    #[test]
    fn amount_overflow_and_missing_denomination_rejected() {
        let (s, sk) = fixture();
        is_error(
            parse(
                &token(
                    &s,
                    vec![proof(&s, &sk, "a", 1 << 63), proof(&s, &sk, "b", 1 << 63)],
                ),
                &s,
            ),
            ValidationError::Overflow,
        );
        is_error(
            parse(&token(&s, vec![proof(&s, &sk, "a", 4)]), &s),
            ValidationError::Denomination,
        );
        is_error(
            parse(&token(&s, vec![proof(&s, &sk, "a", 0)]), &s),
            ValidationError::Denomination,
        );
    }
    #[test]
    fn unknown_keyset_and_invalid_encoding_rejected() {
        let (s, sk) = fixture();
        let mut p = proof(&s, &sk, "a", 1);
        p.keyset_id = Id::from_str("0011223344556677").unwrap();
        assert!(parse(&token(&s, vec![p]), &s).is_err());
        is_error(
            ValidatedToken::parse(b"not-a-token", &s),
            ValidationError::Encoding,
        );
        is_error(parse(&token(&s, vec![]), &s), ValidationError::Empty);
    }
    #[test]
    fn metadata_must_match_unit_and_cryptographic_keyset_id() {
        let (s, _) = fixture();
        let mut keys = s.keysets.clone();
        keys[0].unit = CurrencyUnit::Msat;
        assert!(TrustedMintSnapshot::new(s.mint.clone(), s.unit.clone(), keys).is_err());
        let mut keys = s.keysets.clone();
        keys[0].id = Id::from_str("0011223344556677").unwrap();
        assert!(TrustedMintSnapshot::new(s.mint, s.unit, keys).is_err());
    }
    #[test]
    fn fee_unknown_fails_closed_and_total_is_checked() {
        let (s, sk) = fixture();
        let v = parse(&token(&s, vec![proof(&s, &sk, "a", 2)]), &s).unwrap();
        assert_eq!(
            v.checked_total(FeeKnowledge::Unknown),
            Err(ValidationError::UnknownFee)
        );
        assert_eq!(v.checked_total(FeeKnowledge::TrustedUpperBound(3)), Ok(5));
        assert_eq!(
            v.checked_total(FeeKnowledge::TrustedUpperBound(u64::MAX)),
            Err(ValidationError::Overflow)
        );
    }
}
