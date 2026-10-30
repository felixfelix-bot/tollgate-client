#![cfg(all(feature = "cdk", feature = "persistence", target_os = "linux"))]
use cdk::{
    dhke::{hash_to_curve, sign_message},
    mint_url::MintUrl,
    nuts::{CurrencyUnit, Id, KeySet, Keys, Proof, SecretKey, Token},
    secp256k1::hashes::{sha256, Hash},
    secret::Secret,
    Amount,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::{Command, Output},
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};
use tollgate_client_core::{offline_policy::Enrollment, token_vault::ProtectedTokenVault};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    policy: Enrollment,
    keyset: KeySet,
    sk: SecretKey,
}
impl Fixture {
    fn new(cap: u64) -> Self {
        let root = PathBuf::from("/dev/shm").join(format!(
            "offline-process-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        ProtectedTokenVault::open(&root).unwrap();
        let sk = SecretKey::from_str(&"01".repeat(32)).unwrap();
        let keys = Keys::new(
            [1u64, 2, 8]
                .into_iter()
                .map(|n| (Amount::from(n), sk.public_key()))
                .collect::<BTreeMap<_, _>>(),
        );
        let keyset = KeySet {
            id: Id::v1_from_keys(&keys),
            unit: CurrencyUnit::Sat,
            active: Some(false),
            keys,
            input_fee_ppk: 0,
            final_expiry: None,
        };
        let snapshot = serde_json::to_vec(&serde_json::json!({"mint":"https://mint.example", "unit":"sat", "keysets":[keyset.clone()]})).unwrap();
        std::fs::write(root.join("snapshot.json"), &snapshot).unwrap();
        let policy = Enrollment {
            caps: [("sat".into(), cap)].into(),
            fee: 1,
            snapshot_sha256: sha256::Hash::hash(&snapshot).to_string(),
            valid_from: 10,
            valid_until: 20,
        };
        let f = Self {
            root,
            policy,
            keyset,
            sk,
        };
        f.write_policy();
        f
    }
    fn write_policy(&self) {
        std::fs::write(
            self.root.join("expected.json"),
            serde_json::to_vec(&self.policy).unwrap(),
        )
        .unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        let executable = option_env!("CARGO_BIN_EXE_offline-policy-sim")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/debug/offline-policy-sim"));
        Command::new(executable)
            .current_dir(&self.root)
            .args(args)
            .output()
            .expect("offline-policy-sim executable must exist")
    }
    fn enroll(&self) {
        let o = self.run(&["enroll", "ledger", "expected.json"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    fn token(&self, secret: &str, amount: u64, alias: bool) -> String {
        let secret = Secret::from_str(secret).unwrap();
        let c = sign_message(&self.sk, &hash_to_curve(secret.as_bytes()).unwrap()).unwrap();
        let proof = Proof::new(amount.into(), self.keyset.id, secret, c);
        let token = Token::new(
            MintUrl::from_str(if alias {
                "https://MINT.example:443/"
            } else {
                "https://mint.example"
            })
            .unwrap(),
            vec![proof],
            None,
            CurrencyUnit::Sat,
        );
        let bytes = if alias {
            token.to_v3_string()
        } else {
            token.to_string()
        };
        let vault = ProtectedTokenVault::open(self.root.join("vault")).unwrap();
        let reference = vault.store(bytes.as_bytes()).unwrap();
        reference
            .digest()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    fn attempt(&self, digest: &str, op: &str, now: &str) -> Output {
        self.run(&[
            "attempt",
            "ledger",
            "expected.json",
            "snapshot.json",
            "vault",
            digest,
            op,
            "1",
            now,
            "1",
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn accepted(o: Output, digest: &str, usage: u64) {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        format!("SIMULATION_CALLBACK {digest}\nSIMULATION_USAGE {usage}\n")
    );
    assert!(o.stderr.is_empty());
}
fn refused(o: Output, reason: &str) {
    assert!(!o.status.success());
    assert!(o.stdout.is_empty(), "callback occurred on refusal");
    let error = String::from_utf8(o.stderr).unwrap();
    assert!(error.contains(reason), "{error}");
    assert!(!error.contains("cashu"));
}
#[test]
fn restart_retains_cap_and_counts_face_plus_fee_not_advert() {
    let f = Fixture::new(10);
    f.enroll();
    let a = f.token("large", 8, false);
    accepted(f.attempt(&a, "first", "15"), &a, 9);
    let b = f.token("second", 2, false);
    refused(f.attempt(&b, "second", "15"), "validated send refused");
}
#[test]
fn changed_fee_or_cap_rejected_before_first_send() {
    for fee in [false, true] {
        let mut f = Fixture::new(10);
        f.enroll();
        let a = f.token("a", 2, false);
        if fee {
            f.policy.fee = 0;
        } else {
            f.policy.caps.insert("sat".into(), 100);
        }
        f.write_policy();
        refused(f.attempt(&a, "first", "15"), "enrollment mismatch");
    }
}
#[test]
fn overlapping_alias_and_encoding_never_get_second_callback() {
    let f = Fixture::new(100);
    f.enroll();
    let a = f.token("same-proof", 2, false);
    let b = f.token("same-proof", 2, true);
    assert_ne!(a, b);
    accepted(f.attempt(&a, "first", "15"), &a, 3);
    refused(f.attempt(&b, "alias", "15"), "validated send refused");
    let replay = f.attempt(&a, "first", "15");
    assert!(replay.status.success());
    assert_eq!(replay.stdout, b"SIMULATION_USAGE 3\n");
}
#[test]
fn corrupt_vault_has_zero_callbacks() {
    let f = Fixture::new(10);
    f.enroll();
    let a = f.token("a", 2, false);
    std::fs::write(f.root.join("vault").join(&a), b"corrupt").unwrap();
    refused(f.attempt(&a, "first", "15"), "vault load failed");
}
#[test]
fn wrong_snapshot_hash_and_staleness_fail_closed() {
    let f = Fixture::new(10);
    f.enroll();
    let a = f.token("a", 2, false);
    for now in ["9", "20"] {
        refused(f.attempt(&a, "first", now), "freshness");
    }
    std::fs::write(f.root.join("snapshot.json"), b"{}").unwrap();
    refused(f.attempt(&a, "first", "15"), "digest mismatch");
}
#[test]
fn unenrolled_unit_rejected() {
    let mut f = Fixture::new(10);
    f.policy.caps = [("msat".into(), 100)].into();
    f.write_policy();
    f.enroll();
    let a = f.token("a", 2, false);
    refused(f.attempt(&a, "first", "15"), "unit not enrolled");
}
