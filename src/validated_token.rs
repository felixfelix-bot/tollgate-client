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
