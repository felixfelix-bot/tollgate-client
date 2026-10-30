//! Offline-only CLI boundary. No wallet, HTTP client or legacy runner is imported.
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tollgate_client_core::{
    offline_policy::{Enrollment, PolicyAdapter},
    send_state::DeliveryObservation,
    token_vault::{ProtectedTokenVault, VaultRef},
    validated_send::VerifiedBytesTransport,
};

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Enrolled,
    RecordedUnresolved,
    Unresolved,
    Refused,
}
#[derive(Serialize)]
struct Report<'a> {
    mode: &'static str,
    outcome: Outcome,
    clock: &'static str,
    usage: Option<u64>,
    error: Option<&'a str>,
}
fn report(outcome: Outcome, usage: Option<u64>, error: Option<&str>) {
    println!(
        "{}",
        serde_json::to_string(&Report {
            mode: "simulated",
            outcome,
            clock: "system_unix_seconds_utc_day",
            usage,
            error
        })
        .unwrap()
    );
}
#[derive(Serialize)]
struct Record<'a> {
    mode: &'static str,
    operation: &'a str,
    digest: &'a str,
}
struct Recording<'a> {
    path: PathBuf,
    operation: &'a str,
    digest: &'a str,
    recorded: bool,
    error: bool,
    pause: bool,
}
impl VerifiedBytesTransport for Recording<'_> {
    fn send(&mut self, _body: &[u8]) -> DeliveryObservation {
        // The adapter invokes this only AFTER durable send ownership commits.
        #[cfg(feature = "nonproduction-test-harness")]
        if self.pause {
            eprintln!("SIMULATION_SEND_MARK_DURABLE");
            loop {
                std::thread::park();
            }
        }
        let _ = self.pause;
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&self.path)?;
            let mut bytes = serde_json::to_vec(&Record {
                mode: "simulated",
                operation: self.operation,
                digest: self.digest,
            })?;
            bytes.push(b'\n');
            file.write_all(&bytes)?;
            file.sync_all()?;
            File::open(self.path.parent().unwrap())?.sync_all()
        })();
        self.recorded = result.is_ok();
        self.error = result.is_err();
        DeliveryObservation::Unknown
    }
}
const MARKER: &str = "tollgate-agent-recording-only-v1\n";
fn run(args: &[String]) -> Result<(Outcome, Option<u64>), String> {
    if args.get(1).map(String::as_str) != Some("--recording-only") {
        return Err("recording-only must be explicit and first".into());
    }
    let action = args
        .get(2)
        .map(String::as_str)
        .ok_or("missing recording action")?;
    if !["enroll", "attempt"].contains(&action) {
        return Err("unknown recording action".into());
    }
    let mut opts = BTreeMap::new();
    let mut i = 3;
    #[cfg(feature = "nonproduction-test-harness")]
    let mut pause = false;
    #[cfg(not(feature = "nonproduction-test-harness"))]
    let pause = false;
    while i < args.len() {
        #[cfg(feature = "nonproduction-test-harness")]
        if args[i] == "--simulation-pause-before-record" && action == "attempt" && !pause {
            pause = true;
            i += 1;
            continue;
        }
        let allowed = if action == "enroll" {
            &["--simulation-state", "--simulation-policy"][..]
        } else {
            &[
                "--simulation-state",
                "--simulation-policy",
                "--simulation-snapshot",
                "--simulation-digest",
                "--simulation-operation",
            ][..]
        };
        if !allowed.contains(&args[i].as_str()) {
            return Err("unknown or incompatible recording option".into());
        }
        let value = args
            .get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .ok_or("missing option value")?;
        if opts.insert(args[i].as_str(), value.as_str()).is_some() {
            return Err("duplicate recording option".into());
        }
        i += 2;
    }
    let get = |key| {
        opts.get(key)
            .copied()
            .ok_or_else(|| "missing recording option".to_string())
    };
    let root = Path::new(get("--simulation-state")?);
    let policy: Enrollment = serde_json::from_slice(
        &fs::read(get("--simulation-policy")?).map_err(|_| "policy read failed")?,
    )
    .map_err(|_| "invalid policy JSON")?;
    if action == "enroll" {
        // Refuse even empty pre-existing directories: never adopt a wallet/ledger.
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root)
            .map_err(|_| "simulation directory must be new")?;
        ProtectedTokenVault::open(root).map_err(|_| "unsafe simulation directory")?;
        PolicyAdapter::enroll(&root.join("ledger"), &policy)?;
        let mut marker = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join("SIMULATION_ONLY"))
            .map_err(|_| "marker create failed")?;
        marker
            .write_all(MARKER.as_bytes())
            .and_then(|_| marker.sync_all())
            .map_err(|_| "marker sync failed")?;
        File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(|_| "directory sync failed")?;
        return Ok((Outcome::Enrolled, None));
    }
    let snapshot = fs::read(get("--simulation-snapshot")?).map_err(|_| "snapshot read failed")?;
    let digest = get("--simulation-digest")?;
    let operation = get("--simulation-operation")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid simulation digest".into());
    }
    ProtectedTokenVault::open(root).map_err(|_| "unsafe simulation directory")?;
    if fs::read(root.join("SIMULATION_ONLY")).map_err(|_| "simulation enrollment required")?
        != MARKER.as_bytes()
    {
        return Err("simulation enrollment required".into());
    }
    let mut bytes = [0; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&digest[2 * i..2 * i + 2], 16).map_err(|_| "invalid digest")?;
    }
    let vault = ProtectedTokenVault::open(root.join("vault")).map_err(|_| "vault open failed")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock before epoch")?
        .as_secs();
    let mut recording = Recording {
        path: root.join("recordings.jsonl"),
        operation,
        digest,
        recorded: false,
        error: false,
        pause,
    };
    let usage = PolicyAdapter::open(&root.join("ledger"))?.attempt(
        &policy,
        &snapshot,
        &vault,
        &VaultRef::from_digest(bytes),
        operation,
        now / 86400,
        now,
        &mut recording,
    )?;
    if recording.error {
        return Err("recording failed; send remains unresolved".into());
    }
    Ok((
        if recording.recorded {
            Outcome::RecordedUnresolved
        } else {
            Outcome::Unresolved
        },
        Some(usage),
    ))
}
pub fn main(args: &[String]) -> std::process::ExitCode {
    match run(args) {
        Ok((outcome, usage)) => {
            report(outcome, usage, None);
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            report(Outcome::Refused, None, Some(&e));
            std::process::ExitCode::from(64)
        }
    }
}
