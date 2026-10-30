//! The Debian side of the client: probe a gate, decide, buy, journal.
//!
//! Everything platform-specific is injected — the wallet (Cashu) and the clock —
//! so this runs unchanged on a laptop, a VPS, or a router with a shell.

use crate::agent::{attempt_purchase, AttemptResult, GateTransport, Journal, Wallet};
use crate::discovery::parse_advertisement;
use crate::http;
use crate::policy::{decide, Caps};
use crate::purchase::IntentState;
use crate::scoring::{GatewayHistory, Offer};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Transport that talks to the gate. No TLS: the v1 gate is plain HTTP.
pub struct HttpGate {
    pub timeout_ms: u64,
}

impl GateTransport for HttpGate {
    fn post_token(&mut self, gate_url: &str, token: &str) -> Result<(u16, String), String> {
        http::request("POST", gate_url, Some(token), self.timeout_ms)
    }
}

/// A wallet backed by one pre-minted token file.
///
/// This is the honest MVP: the CDK wallet (mint quote → swap → token) is a
/// separate deliverable, and pretending a file is a wallet would be worse than
/// naming the boundary. `is_spent` is unsupported here, so an ambiguous answer
/// surfaces as an error instead of a guess — which is the correct failure.
pub struct FileWallet {
    pub token: String,
}

impl Wallet for FileWallet {
    fn mint_token(&mut self, _mint_url: &str, _unit: &str, _amount: u64) -> Result<String, String> {
        Ok(self.token.clone())
    }
    fn is_spent(&mut self, _token: &str) -> Result<bool, String> {
        Err("FileWallet cannot answer NUT-07; an ambiguous answer must be resolved by a real wallet".into())
    }
}

/// Append-only JSON-lines journal: one line per key, rewritten in place on a
/// state change. Small enough to keep the whole map in memory.
#[derive(Default)]
pub struct FileJournal {
    pub path: PathBuf,
    pub states: HashMap<String, IntentState>,
}

impl FileJournal {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let mut states = HashMap::new();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once(' ') {
                    if let Some(state) = IntentState::parse(v) {
                        states.insert(k.to_string(), state);
                    }
                }
            }
        }
        Ok(Self { path, states })
    }
}

impl Journal for FileJournal {
    fn begin(&mut self, key: &str) -> Result<(), String> {
        if self.states.contains_key(key) {
            return Err("already attempted".into());
        }
        self.states.insert(key.to_string(), IntentState::Pending);
        self.append(key, IntentState::Pending)
    }
    fn set_state(&mut self, key: &str, state: IntentState) -> Result<(), String> {
        self.states.insert(key.to_string(), state);
        self.append(key, state)
    }
    fn state(&self, key: &str) -> Option<IntentState> {
        self.states.get(key).copied()
    }
}

impl FileJournal {
    fn append(&self, key: &str, state: IntentState) -> Result<(), String> {
        if let Some(dir) = Path::new(&self.path).parent() {
            fs::create_dir_all(dir).map_err(|e| format!("journal dir {}: {e}", dir.display()))?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("journal {}: {e}", self.path.display()))?;
        writeln!(f, "{key} {}", state.as_str()).map_err(|e| e.to_string())
    }
}

pub struct RunReport {
    pub gate_url: String,
    pub advert_offers: usize,
    pub decision: String,
    pub result: Option<AttemptResult>,
    /// Latency and throughput of the advert probe itself. Free measurement: we
    /// already pay for this round trip, so the quality sample comes from it
    /// rather than from a second poll.
    pub probe_rtt_ms: f64,
    pub probe_kbps: f64,
}

/// Probe, decide, buy. `attempt_id` must be unique per attempt (the caller's clock
/// or a counter); it keys the journal, so a retry of the *same* attempt is refused.
pub fn run_once(
    gate_url: &str,
    caps: &Caps,
    histories: &HashMap<String, GatewayHistory>,
    wallet: &mut dyn Wallet,
    journal: &mut dyn Journal,
    attempt_id: &str,
    timeout_ms: u64,
) -> Result<RunReport, String> {
    let probe_started = std::time::Instant::now();
    let (status, body) = http::request("GET", gate_url, None, timeout_ms)?;
    let probe_rtt_ms = probe_started.elapsed().as_secs_f64() * 1000.0;
    let probe_kbps = if probe_rtt_ms > 0.0 {
        (body.len() as f64 * 8.0) / probe_rtt_ms
    } else {
        0.0
    };
    if status != 200 {
        return Err(format!(
            "{gate_url} answered HTTP {status} on the advert probe"
        ));
    }
    let kind = body
        .split("\"kind\":")
        .nth(1)
        .and_then(|r| {
            r.trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .ok()
        })
        .ok_or_else(|| "advert body has no numeric kind".to_string())?;
    let tags = extract_tags(&body);
    let advert = parse_advertisement(kind, &tags).map_err(|e| e.to_string())?;

    let mut stamped = advert.clone();
    stamped.stamp_gateway(gate_url);
    let offers = stamped.offers.clone();

    let decision = decide(&offers, histories, caps, None)?;
    let reason = format!("{decision:?}");
    let outcome = match &decision {
        crate::policy::Decision::Buy { gateway_id, .. } => {
            let offer: Offer = offers
                .iter()
                .find(|o| &o.gateway_id == gateway_id)
                .cloned()
                .ok_or_else(|| "selected offer vanished".to_string())?;
            let mut transport = HttpGate { timeout_ms };
            Some(attempt_purchase(
                gateway_id,
                &offer,
                caps,
                attempt_id,
                wallet,
                &mut transport,
                journal,
            )?)
        }
        _ => None,
    };

    Ok(RunReport {
        gate_url: gate_url.to_string(),
        advert_offers: offers.len(),
        decision: reason,
        result: outcome,
        probe_rtt_ms,
        probe_kbps,
    })
}

/// One gate, summarised as JSON for a UI. Kept here rather than in the Android
/// crate so the phone and the CLI cannot disagree about what an offer costs.
pub fn describe_gate(
    gate_url: &str,
    caps: &Caps,
    histories: &HashMap<String, crate::scoring::GatewayHistory>,
    timeout_ms: u64,
) -> Result<String, String> {
    let (status, body) = http::request("GET", gate_url, None, timeout_ms)?;
    if status != 200 {
        return Err(format!("{gate_url} answered HTTP {status}"));
    }
    let kind = body
        .split("\"kind\":")
        .nth(1)
        .and_then(|r| {
            r.trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .ok()
        })
        .ok_or_else(|| "advert body has no numeric kind".to_string())?;
    let mut advert = parse_advertisement(kind, &extract_tags(&body)).map_err(|e| e.to_string())?;
    advert.stamp_gateway(gate_url);
    let decision = decide(&advert.offers, histories, caps, None)?;

    let offers: Vec<String> = advert
        .offers
        .iter()
        .map(|o| {
            let h = histories.get(&o.gateway_id);
            format!(
                r#"{{"unit":"{}","price_per_step":{},"steps":{},"cost":{:.3},"cost_per_minute":{},"quality":{:.3},"p":{:.3}}}"#,
                o.unit,
                o.price_per_step,
                o.steps_to_buy(),
                o.total_cost(),
                o.cost_per_minute().map(|c| format!("{c:.3}")).unwrap_or("null".into()),
                h.map(|x| x.quality()).unwrap_or(0.5),
                h.map(|x| x.success_probability()).unwrap_or(0.5),
            )
        })
        .collect();
    // The decision is a Rust enum, not a JSON type: render it as a *string* so the
    // whole document still parses. A UI that receives invalid JSON is a bug that
    // hides until someone points a parser at it.
    let decision = format!("{decision:?}")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    Ok(format!(
        r#"{{"gate":"{}","offers":[{}],"decision":"{}"}}"#,
        gate_url,
        offers.join(","),
        decision
    ))
}

/// Probe, decide, buy — and write down what happened.
///
/// The scores are only as good as the events behind them, so every run records a
/// probe sample (it already paid for that round trip) plus the purchase outcome.
/// Histories are loaded from the same file, which is what makes the 20 %
/// hysteresis and the success probability mean anything across restarts.
pub fn run_recorded(
    gate_url: &str,
    caps: &Caps,
    store: &mut crate::history::Store,
    wallet: &mut dyn Wallet,
    journal: &mut dyn Journal,
    attempt_id: &str,
    timeout_ms: u64,
) -> Result<RunReport, String> {
    use crate::history::Event;
    use crate::purchase::IntentState;

    let histories = store.load();
    let outcome = run_once(
        gate_url, caps, &histories, wallet, journal, attempt_id, timeout_ms,
    );

    match &outcome {
        Ok(report) => {
            store.record(&Event::Probe {
                gateway_id: report.gate_url.clone(),
                rtt_ms: report.probe_rtt_ms,
                kbps: report.probe_kbps,
            })?;
            match &report.result {
                Some(AttemptResult::Finished { key, state, .. }) => {
                    if *state == IntentState::Granted {
                        store.record(&Event::Granted {
                            gateway_id: report.gate_url.clone(),
                        })?;
                    } else {
                        store.record(&Event::Refused {
                            gateway_id: report.gate_url.clone(),
                            code: format!("{}({key})", state.as_str()),
                        })?;
                    }
                }
                Some(AttemptResult::AlreadyAttempted { .. })
                | Some(AttemptResult::NotAttempted { .. })
                | None => {}
            }
        }
        // Only "we could not reach it" is evidence about the gateway. A malformed
        // advert is evidence about the gate's software, and is not counted here.
        Err(e) if e.starts_with("connect ") => {
            store.record(&Event::Unreachable {
                gateway_id: gate_url.to_string(),
            })?;
        }
        Err(_) => {}
    }

    outcome
}

/// Pull `["name","v","v"]` arrays out of an event body. The core parser wants
/// tags as `Vec<Vec<String>>`; this is the one place that knows JSON shape.
fn extract_tags(body: &str) -> Vec<Vec<String>> {
    let mut tags = Vec::new();
    let Some(start) = body.find("\"tags\"") else {
        return tags;
    };
    let rest = &body[start..];
    let mut depth = 0usize;
    let mut current: Vec<String> = Vec::new();
    let mut in_str = false;
    let mut escaped = false;
    let mut buf = String::new();
    let mut started = false;

    for ch in rest.chars() {
        match ch {
            '"' if !escaped => {
                in_str = !in_str;
                if !in_str && started {
                    current.push(std::mem::take(&mut buf));
                }
            }
            '\\' if in_str => {
                escaped = !escaped;
                continue;
            }
            '[' if !in_str => {
                depth += 1;
                if depth == 2 {
                    started = true;
                    current.clear();
                    // The key itself (`"tags"`) was accumulated into `buf` before
                    // the array opened; without this the first tag name reads
                    // "tagsmetric" and every lookup misses.
                    buf.clear();
                }
            }
            ']' if !in_str => {
                if depth == 2 && started {
                    if !current.is_empty() {
                        tags.push(std::mem::take(&mut current));
                    }
                    started = false;
                }
                depth = depth.saturating_sub(1);
            }
            _ => {
                if in_str {
                    buf.push(ch);
                }
            }
        }
        escaped = false;
    }
    let _ = &mut current;
    tags
}

#[cfg(test)]
mod tests {
    #[test]
    fn extracts_tag_arrays_from_an_event_body() {
        let body = r#"{"kind":10021,"tags":[["metric","milliseconds"],["step_size","60000"]]}"#;
        let tags = super::extract_tags(body);
        eprintln!("TAGS = {tags:?}");
        assert_eq!(tags.len(), 2);
        assert_eq!(
            tags[0],
            vec!["metric".to_string(), "milliseconds".to_string()]
        );
    }
}

/// The Lightning lane, end to end: probe the advert, choose an offer, let the
/// gate mint the invoice, pay it, poll until the gate opens.
///
/// No token is ever minted or held by the client on this path — which is why it
/// works with no wallet at all.
pub struct LightningReport {
    pub advert_offers: usize,
    pub decision: String,
    pub status: Option<crate::lightning::InvoiceStatus>,
}

pub fn run_lightning(
    gate_url: &str,
    caps: &Caps,
    histories: &HashMap<String, GatewayHistory>,
    payer: &mut dyn crate::lightning::LightningPayer,
    timeout_ms: u64,
    poll_interval_ms: u64,
    max_polls: u32,
) -> Result<LightningReport, String> {
    let (status_code, body) = http::request("GET", gate_url, None, timeout_ms)?;
    if status_code != 200 {
        return Err(format!(
            "{gate_url} answered HTTP {status_code} on the advert probe"
        ));
    }
    let kind = body
        .split("\"kind\":")
        .nth(1)
        .and_then(|r| {
            r.trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .ok()
        })
        .ok_or_else(|| "advert body has no numeric kind".to_string())?;
    let mut advert = parse_advertisement(kind, &extract_tags(&body)).map_err(|e| e.to_string())?;
    advert.stamp_gateway(gate_url);

    let decision = decide(&advert.offers, histories, caps, None)?;
    let reason = format!("{decision:?}");
    let status = match &decision {
        crate::policy::Decision::Buy {
            gateway_id,
            steps,
            cost,
            ..
        } => {
            let offer = advert
                .offers
                .iter()
                .find(|o| &o.gateway_id == gateway_id)
                .ok_or_else(|| "selected offer vanished".to_string())?;
            let _ = (steps, cost);
            Some(
                crate::lightning::buy(
                    gate_url,
                    offer.total_cost().round() as u64,
                    &offer.mint_url,
                    payer,
                    timeout_ms,
                    poll_interval_ms,
                    max_polls,
                )
                .map_err(|e| e.to_string())?,
            )
        }
        _ => None,
    };

    Ok(LightningReport {
        advert_offers: advert.offers.len(),
        decision: reason,
        status,
    })
}
