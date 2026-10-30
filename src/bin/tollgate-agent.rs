//! `tollgate-agent` — probe a gate, decide, buy, journal.
//!
//! Usage:
//!   tollgate-agent --gate http://<router-ip>:2121 \
//!                  --token-file /run/secrets/note \
//!                  --journal ~/.tollgate/journal.txt \
//!                  --attempt-id "$(date +%s)" \
//!                  [--max-cost-per-minute 2] [--daily-budget 100] \
//!                  [--spent-today 0] [--unit sat] [--timeout-ms 3000]

use std::collections::HashMap;
use std::process::ExitCode;
use tollgate_client_core::agent::Wallet;
#[cfg(feature = "cdk")]
use tollgate_client_core::cdk_wallet::CdkFunding;
use tollgate_client_core::lightning::LightningPayer;
use tollgate_client_core::policy::Caps;
use tollgate_client_core::purchase::IntentState;
use tollgate_client_core::runner::{
    run_lightning, run_once, run_recorded, FileJournal, FileWallet,
};

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

/// A test mint pays its own invoices. Paying "nothing" is only honest when the
/// mint is the one settling it, so this payer says so in its answer and waits
/// for the quote to flip before cdk asks for the proofs.
struct TestMintPayer;

impl LightningPayer for TestMintPayer {
    fn pay(&mut self, bolt11: &str, amount_sat: u64) -> Result<String, String> {
        eprintln!(
            "test-mint: {amount_sat} sat invoice not paid by us (the mint settles it): {}…",
            &bolt11[..bolt11.len().min(24)]
        );
        std::thread::sleep(std::time::Duration::from_secs(6));
        Ok("test-mint".to_string())
    }
}

/// The cdk wallet, or a clear refusal in a build that was compiled without it.
#[cfg(feature = "cdk")]
fn cdk_wallet_from_args(args: &[String]) -> Result<Box<dyn Wallet>, (String, u8)> {
    let Some(seed_file) = opt(args, "--seed-file", "TOLLGATE_SEED_FILE") else {
        return Err((
            "--wallet cdk needs --seed-file (64 raw bytes, mode 0600)".into(),
            64,
        ));
    };
    let Some(db_dir) = opt(args, "--wallet-db", "TOLLGATE_WALLET_DB") else {
        return Err((
            "--wallet cdk needs --wallet-db (a directory, 0700)".into(),
            64,
        ));
    };
    let seed = load_seed(&seed_file).map_err(|e| (e, 78))?;
    let payer: Box<dyn LightningPayer> = if args.iter().any(|a| a == "--test-mint") {
        Box::new(TestMintPayer)
    } else {
        Box::new(ManualPayer)
    };
    let margin = opt(args, "--token-margin", "TOLLGATE_TOKEN_MARGIN")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Ok(Box::new(
        CdkFunding::new(seed, db_dir, payer).with_margin(margin),
    ))
}

#[cfg(not(feature = "cdk"))]
fn cdk_wallet_from_args(_args: &[String]) -> Result<Box<dyn Wallet>, (String, u8)> {
    Err((
        "this binary was built without the cdk wallet (--no-default-features)".into(),
        78,
    ))
}

/// A wallet seed is the wallet. Anything but `0600` is refused rather than
/// silently accepted, because the failure mode is theft, not breakage.
#[cfg(feature = "cdk")]
fn load_seed(path: &str) -> Result<[u8; 64], String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot stat seed file {path}: {e}"))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(format!(
            "seed file {path} is mode {mode:o}; refusing anything but 0600"
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read seed file {path}: {e}"))?;
    if bytes.len() != 64 {
        return Err(format!(
            "seed file {path} is {} bytes; expected 64",
            bytes.len()
        ));
    }
    let mut seed = [0u8; 64];
    seed.copy_from_slice(&bytes);
    Ok(seed)
}

/// "There is nothing to buy here" is not an error. Distinguishing the two is
/// what keeps `systemctl status` honest: red only when something is wrong.
fn not_a_gate(error: &str) -> bool {
    error.starts_with("connect ")                       // refused / unreachable
        || (error.contains("answered HTTP") && error.contains("advert probe")) // 404/502, a normal web server
}

/// Flag first, then the environment, so systemd's EnvironmentFile is load-bearing
/// rather than decorative.
fn opt(args: &[String], flag: &str, env_key: &str) -> Option<String> {
    arg(args, flag).or_else(|| std::env::var(env_key).ok().filter(|v| !v.trim().is_empty()))
}

fn lightning_main(args: &[String], gate: &str) -> ExitCode {
    if arg(args, "--amount").is_some() {
        eprintln!("note: --amount is ignored; the price comes from the advert");
    }
    let mut caps = Caps {
        preferred_unit: arg(args, "--unit").unwrap_or_else(|| "sat".to_string()),
        max_cost_per_minute: opt(
            args,
            "--max-cost-per-minute",
            "TOLLGATE_MAX_COST_PER_MINUTE",
        )
        .and_then(|v| v.parse().ok())
        .unwrap_or(2.0),
        daily_budget: opt(args, "--daily-budget", "TOLLGATE_DAILY_BUDGET")
            .and_then(|v| v.parse().ok())
            .unwrap_or(100.0),
        spent_today: arg(args, "--spent-today")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        switch_margin: 0.20,
    };
    let timeout_ms = arg(args, "--timeout-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let poll_ms = arg(args, "--poll-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let max_polls = arg(args, "--max-polls")
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let histories: HashMap<String, _> = HashMap::new();
    let mut payer = ManualPayer;

    let _ = &mut caps;
    match run_lightning(
        gate, &caps, &histories, &mut payer, timeout_ms, poll_ms, max_polls,
    ) {
        Ok(report) => {
            println!(
                "PROBE gate={} offers={} decision={}",
                gate, report.advert_offers, report.decision
            );
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
        Err(e) if not_a_gate(&e) => {
            println!("RESULT state=not-a-gate reason={e}");
            ExitCode::SUCCESS
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

    // The Lightning lane needs no wallet, so it must be reachable from the
    // packaged service without a token file: TOLLGATE_MODE=lightning.
    let lightning_mode = args.iter().any(|a| a == "--lightning")
        || std::env::var("TOLLGATE_MODE")
            .map(|m| m == "lightning")
            .unwrap_or(false);
    // --auto asks the kernel where traffic is actually going: a gate is only a
    // gate if the default route points at it.
    let gate = match arg(&args, "--gate") {
        Some(g) => g,
        None if args.iter().any(|a| a == "--auto") => {
            match tollgate_client_core::netcfg::default_gateway() {
                Some(gw) => {
                    let url = tollgate_client_core::netcfg::gate_url(&gw);
                    eprintln!("AUTO gateway {gw} -> {url}");
                    url
                }
                None => {
                    // Not a failure: there is simply no link to pay for.
                    println!("RESULT state=no-gateway");
                    return ExitCode::SUCCESS;
                }
            }
        }
        None => {
            eprintln!("missing --gate (or --auto); --help for usage");
            return ExitCode::from(64);
        }
    };

    if lightning_mode {
        return lightning_main(&args, &gate);
    }

    let (Some(journal), Some(attempt_id)) = (arg(&args, "--journal"), arg(&args, "--attempt-id"))
    else {
        eprintln!(
            "missing required flag (--token-file, --journal, --attempt-id); --help for usage"
        );
        return ExitCode::from(64);
    };
    let token_file = opt(&args, "--token-file", "TOLLGATE_TOKEN_FILE").unwrap_or_default();
    if opt(&args, "--wallet", "TOLLGATE_WALLET").as_deref() != Some("cdk") && token_file.is_empty()
    {
        eprintln!("missing --token-file (or use --wallet cdk); --help for usage");
        return ExitCode::from(64);
    }

    // Two funding sources behind one seam: a pre-funded note, or cdk minting on
    // demand (which pays the mint's BOLT11 through the payer before it returns).
    let mut wallet: Box<dyn Wallet> =
        if opt(&args, "--wallet", "TOLLGATE_WALLET").as_deref() == Some("cdk") {
            match cdk_wallet_from_args(&args) {
                Ok(w) => w,
                Err((message, code)) => {
                    eprintln!("{message}");
                    return ExitCode::from(code);
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
        max_cost_per_minute: opt(
            &args,
            "--max-cost-per-minute",
            "TOLLGATE_MAX_COST_PER_MINUTE",
        )
        .and_then(|v| v.parse().ok())
        .unwrap_or(2.0),
        daily_budget: opt(&args, "--daily-budget", "TOLLGATE_DAILY_BUDGET")
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

    // With --history the run both *uses* the record and *extends* it, which is
    // what makes reliability and quality mean something across restarts.
    let mut store =
        opt(&args, "--history", "TOLLGATE_HISTORY").map(tollgate_client_core::history::Store::open);
    let run = match store.as_mut() {
        Some(s) => run_recorded(
            &gate,
            &caps,
            s,
            wallet.as_mut(),
            &mut journal,
            &attempt_id,
            timeout_ms,
        ),
        None => run_once(
            &gate,
            &caps,
            &histories,
            wallet.as_mut(),
            &mut journal,
            &attempt_id,
            timeout_ms,
        ),
    };
    if let Some(s) = store.as_mut() {
        for (id, h) in s.load() {
            println!(
                "HISTORY gw={id} ok={} fail={} rtt_ms={:.1} kbps={:.0} p={:.3}",
                h.successes,
                h.failures,
                h.rtt_ms,
                h.kbps,
                h.success_probability()
            );
        }
        if s.skipped() > 0 {
            eprintln!("warning: {} unreadable history line(s)", s.skipped());
        }
    }

    match run {
        Ok(report) => {
            println!(
                "PROBE gate={} offers={} decision={}",
                report.gate_url, report.advert_offers, report.decision
            );
            match report.result {
                Some(AttemptResult::Finished {
                    key,
                    state,
                    outcome,
                    cost,
                }) => {
                    println!(
                        "RESULT key={key} state={} cost={cost} outcome={outcome:?}",
                        state.as_str()
                    );
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
                    println!(
                        "RESULT key={key} state={} reason=already-attempted",
                        state.as_str()
                    );
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
