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
use tollgate_client_core::runner::{run_once, FileJournal, FileWallet};
use tollgate_client_core::agent::AttemptResult;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("usage: tollgate-agent --gate <url> --token-file <path> --journal <path> --attempt-id <id> \
[--max-cost-per-minute N] [--daily-budget N] [--spent-today N] [--unit sat] [--timeout-ms N]");
        return ExitCode::SUCCESS;
    }

    let (Some(gate), Some(token_file), Some(journal), Some(attempt_id)) = (
        arg(&args, "--gate"),
        arg(&args, "--token-file"),
        arg(&args, "--journal"),
        arg(&args, "--attempt-id"),
    ) else {
        eprintln!("missing required flag (--gate, --token-file, --journal, --attempt-id); --help for usage");
        return ExitCode::from(64);
    };

    let token = match std::fs::read_to_string(&token_file) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            eprintln!("cannot read token file {token_file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut wallet = FileWallet { token };
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
