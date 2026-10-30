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
            "cli-recording-{}-{}",
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
            valid_from: 0,
            valid_until: u64::MAX,
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
        Command::new(env!("CARGO_BIN_EXE_tollgate-agent"))
            .current_dir(&self.root)
            .args(args)
            .env("TOLLGATE_MODE", "lightning")
            .env("TOLLGATE_WALLET", "cdk")
            .output()
            .unwrap()
    }
    fn enroll(&self) {
        let o = self.run(&[
            "--recording-only",
            "enroll",
            "--simulation-state",
            "simulation",
            "--simulation-policy",
            "expected.json",
        ]);
        assert!(o.status.success(), "{:?}", o);
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
        let vault = ProtectedTokenVault::open(self.root.join("simulation/vault")).unwrap();
        let reference = vault.store(bytes.as_bytes()).unwrap();
        reference
            .digest()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    fn attempt(&self, digest: &str, op: &str) -> Output {
        self.run(&[
            "--recording-only",
            "attempt",
            "--simulation-state",
            "simulation",
            "--simulation-policy",
            "expected.json",
            "--simulation-snapshot",
            "snapshot.json",
            "--simulation-digest",
            digest,
            "--simulation-operation",
            op,
        ])
    }
    fn records(&self) -> usize {
        std::fs::read_to_string(self.root.join("simulation/recordings.jsonl"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn result(o: Output) -> serde_json::Value {
    assert!(o.status.success(), "{:?}", o);
    println!("{}", String::from_utf8_lossy(&o.stdout).trim());
    serde_json::from_slice(&o.stdout).unwrap()
}
#[test]
fn restart_proof_dedup_and_budget() {
    let f = Fixture::new(10);
    f.enroll();
    let a = f.token("same", 8, false);
    let r = result(f.attempt(&a, "first"));
    assert_eq!(r["mode"], "simulated");
    assert_eq!(r["outcome"], "recorded_unresolved");
    assert_eq!(r["usage"], 9);
    assert_eq!(result(f.attempt(&a, "first"))["outcome"], "unresolved");
    let alias = f.token("same", 8, true);
    assert!(!f.attempt(&alias, "alias").status.success());
    let b = f.token("second", 2, false);
    assert!(!f.attempt(&b, "second").status.success());
    assert_eq!(f.records(), 1);
}
#[test]
fn policy_freshness_and_isolation() {
    let mut f = Fixture::new(10);
    f.enroll();
    let a = f.token("a", 2, false);
    f.policy.fee = 0;
    f.write_policy();
    assert!(!f.attempt(&a, "a").status.success());
    assert_eq!(f.records(), 0);
    assert!(!f
        .run(&[
            "--recording-only",
            "enroll",
            "--simulation-state",
            "simulation",
            "--simulation-policy",
            "expected.json"
        ])
        .status
        .success());
    let mut stale = Fixture::new(10);
    stale.policy.valid_until = 1;
    stale.write_policy();
    stale.enroll();
    let a = stale.token("a", 2, false);
    assert!(!stale.attempt(&a, "a").status.success());
    assert_eq!(stale.records(), 0);
}
#[test]
fn strict_options_and_no_legacy_override() {
    let f = Fixture::new(10);
    for flag in [
        "--lightning",
        "--wallet",
        "--auto",
        "--gate",
        "--daily-budget",
        "--now",
        "--simulation-unknown",
    ] {
        let o = f.run(&[
            "--recording-only",
            "enroll",
            "--simulation-state",
            "simulation",
            "--simulation-policy",
            "expected.json",
            flag,
            "cdk",
        ]);
        assert!(!o.status.success());
        assert!(!f.root.join("simulation").exists());
    }
    let o = f.run(&["--simulation-state", "simulation"]);
    assert!(!o.status.success());
}
#[cfg(feature = "nonproduction-test-harness")]
#[test]
fn kill_after_send_mark_before_record() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let f = Fixture::new(10);
    f.enroll();
    let a = f.token("kill", 2, false);
    let mut child = Command::new(env!("CARGO_BIN_EXE_tollgate-agent"))
        .current_dir(&f.root)
        .args([
            "--recording-only",
            "attempt",
            "--simulation-state",
            "simulation",
            "--simulation-policy",
            "expected.json",
            "--simulation-snapshot",
            "snapshot.json",
            "--simulation-digest",
            &a,
            "--simulation-operation",
            "kill",
            "--simulation-pause-before-record",
        ])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "SIMULATION_SEND_MARK_DURABLE");
    child.kill().unwrap();
    child.wait().unwrap();
    let r = result(f.attempt(&a, "kill"));
    assert_eq!(r["outcome"], "unresolved");
    assert_eq!(r["usage"], 3);
    assert_eq!(f.records(), 0);
}
