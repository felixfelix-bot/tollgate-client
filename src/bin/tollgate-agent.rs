//! `tollgate-agent` — probe a gate, decide, buy, journal.
//!
//! Usage:
//!   tollgate-agent --gate http://192.168.50.205:2121 \
//!                  --token-file /run/secrets/note \
//!                  --journal ~/.tollgate/journal.txt \
//!                  --attempt-id "$(date +%s)" \
//!                  [--max-cost-per-minute 2] [--daily-budget 100] \
//!                  [--spent-today 0] [--unit sat] [--timeout-ms 3000]

use std::collections::HashMap;
use std::process::ExitCode;
use tollgate_client_core::policy::Caps;
use tollgate_client_core::purchase::IntentState;
use tollgate_client_core::lightning::LightningPayer;
use tollgate_client_core::agent::Wallet;
use tollgate_client_core::cdk_wallet::CdkFunding;
use tollgate_client_core::runner::{run_lightning, run_once, FileJournal, FileWallet};

/// Prints the invoice for a human to pay with any wallet, then lets the poll
/// loop find the grant. A real payer (an LN node over NIP-47/REST) implements the
/// same trait; nothing else in the client changes.
struct ManualPayer;

impl LightningPayer for ManualPayer {
    fn pay(&mut self, bolt11: &str, amount_sat: u64) -> Result<String, String> {
        eprintln!("PAY THIS INVOICE ({amount_sat} sat):\n\n{bolt11}\n");
        Ok("manual".to_string())
    }
}
use tollgate_client_core::agent::AttemptResult;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// A wallet seed is the wallet. Anything but `0600` is refused rather than
/// silently accepted, because the failure mode is theft, not breakage.
fn load_seed(path: &str) -> Result<[u8; 64], String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot stat seed file {path}: {e}"))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(format!("seed file {path} is mode {mode:o}; refusing anything but 0600"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read seed file {path}: {e}"))?;
    if bytes.len() != 64 {
        return Err(format!("seed file {path} is {} bytes; expected 64", bytes.len()));
    }
    let mut seed = [0u8; 64];
    seed.copy_from_slice(&bytes);
    Ok(seed)
}

fn lightning_main(args: &[String], gate: &str) -> ExitCode {
    if arg(args, "--amount").is_some() {
        eprintln!("note: --amount is ignored; the price comes from the advert");
    }
    let mut caps = Caps {
        preferred_unit: arg(args, "--unit").unwrap_or_else(|| "sat".to_string()),
        max_cost_per_minute: arg(args, "--max-cost-per-minute")
            .and_then(|v| v.parse().ok())
            .unwrap_or(2.0),
        daily_budget: arg(args, "--daily-budget")
            .and_then(|v| v.parse().ok())
            .unwrap_or(100.0),
        spent_today: arg(args, "--spent-today")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        switch_margin: 0.20,
    };
    let timeout_ms = arg(args, "--timeout-ms").and_then(|v| v.parse().ok()).unwrap_or(3000);
    let poll_ms = arg(args, "--poll-ms").and_then(|v| v.parse().ok()).unwrap_or(1000);
    let max_polls = arg(args, "--max-polls").and_then(|v| v.parse().ok()).unwrap_or(120);
    let histories: HashMap<String, _> = HashMap::new();
    let mut payer = ManualPayer;

    let _ = &mut caps;
    match run_lightning(gate, &caps, &histories, &mut payer, timeout_ms, poll_ms, max_polls) {
        Ok(report) => {
            println!("PROBE gate={} offers={} decision={}", gate, report.advert_offers, report.decision);
            match report.status {
                Some(s) if s.access_granted => {
                    println!(
                        "RESULT state=granted allotment={} metric={}",
                        s.allotment.unwrap_or(0),
                        s.metric.unwrap_or_default()
                    );
                    ExitCode::SUCCESS
                }
                Some(s) => {
                    println!("RESULT state=paid-not-granted gate_state={}", s.state);
                    ExitCode::from(2)
                }
                None => {
                    println!("RESULT state=not-attempted");
                    ExitCode::from(2)
                }
            }
        }
        Err(e) => {
            println!("ERROR {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("usage: tollgate-agent --gate <url> --token-file <path> --journal <path> --attempt-id <id> \
[--max-cost-per-minute N] [--daily-budget N] [--spent-today N] [--unit sat] [--timeout-ms N]");
        return ExitCode::SUCCESS;
    }

    let lightning_mode = args.iter().any(|a| a == "--lightning");
    let Some(gate) = arg(&args, "--gate") else {
        eprintln!("missing --gate; --help for usage");
        return ExitCode::from(64);
    };

    if lightning_mode {
        return lightning_main(&args, &gate);
    }

    let (Some(journal), Some(attempt_id)) = (arg(&args, "--journal"), arg(&args, "--attempt-id")) else {
        eprintln!("missing required flag (--token-file, --journal, --attempt-id); --help for usage");
        return ExitCode::from(64);
    };
    let token_file = arg(&args, "--token-file").unwrap_or_default();
    if arg(&args, "--wallet").as_deref() != Some("cdk") && token_file.is_empty() {
        eprintln!("missing --token-file (or use --wallet cdk); --help for usage");
        return ExitCode::from(64);
    }

    // Two funding sources behind one seam: a pre-funded note, or cdk minting on
    // demand (which pays the mint's BOLT11 through the payer before it returns).
    let mut wallet: Box<dyn Wallet> = if arg(&args, "--wallet").as_deref() == Some("cdk") {
        let Some(seed_file) = arg(&args, "--seed-file") else {
            eprintln!("--wallet cdk needs --seed-file (64 raw bytes, mode 0600)");
            return ExitCode::from(64);
        };
        let Some(db_dir) = arg(&args, "--wallet-db") else {
            eprintln!("--wallet cdk needs --wallet-db (a directory, 0700)");
            return ExitCode::from(64);
        };
        match load_seed(&seed_file) {
            Ok(seed) => Box::new(CdkFunding::new(seed, db_dir, Box::new(ManualPayer))),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(78);
            }
        }
    } else {
        let token = match std::fs::read_to_string(&token_file) {
            Ok(t) => t.trim().to_string(),
            Err(e) => {
                eprintln!("cannot read token file {token_file}: {e}");
                return ExitCode::from(66);
            }
        };
        Box::new(FileWallet { token })
    };
    let mut journal = match FileJournal::open(&journal) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("cannot open journal: {e}");
            return ExitCode::FAILURE;
        }
    };

    let caps = Caps {
        preferred_unit: arg(&args, "--unit").unwrap_or_else(|| "sat".to_string()),
        max_cost_per_minute: arg(&args, "--max-cost-per-minute")
            .and_then(|v| v.parse().ok())
            .unwrap_or(2.0),
        daily_budget: arg(&args, "--daily-budget")
            .and_then(|v| v.parse().ok())
            .unwrap_or(100.0),
        spent_today: arg(&args, "--spent-today")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        switch_margin: 0.20,
    };
    let timeout_ms = arg(&args, "--timeout-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let histories: HashMap<String, _> = HashMap::new();

    match run_once(&gate, &caps, &histories, &mut wallet, &mut journal, &attempt_id, timeout_ms) {
        Ok(report) => {
            println!(
                "PROBE gate={} offers={} decision={}",
                report.gate_url, report.advert_offers, report.decision
            );
            match report.result {
                Some(AttemptResult::Finished { key, state, outcome, cost }) => {
                    println!("RESULT key={key} state={} cost={cost} outcome={outcome:?}", state.as_str());
                    match state {
                        IntentState::Granted => ExitCode::SUCCESS,
                        _ => ExitCode::from(2),
                    }
                }
                Some(AttemptResult::NotAttempted { reason }) => {
                    println!("RESULT state=not-attempted reason={reason}");
                    ExitCode::from(2)
                }
                Some(AttemptResult::AlreadyAttempted { key, state }) => {
                    println!("RESULT key={key} state={} reason=already-attempted", state.as_str());
                    ExitCode::from(2)
                }
                None => {
                    println!("RESULT state=not-attempted");
                    ExitCode::from(2)
                }
            }
        }
        Err(e) => {
            println!("ERROR {e}");
            ExitCode::FAILURE
        }
    }
}
