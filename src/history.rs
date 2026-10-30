//! The memory the scores are computed from.
//!
//! `GatewayHistory` holds EWMA state; this holds the *events* that produced it,
//! append-only, one JSON object per line. Two reasons for event-sourcing rather
//! than dumping the structs:
//!
//!   * an append is atomic-ish with `O_APPEND` and never rewrites the file, so a
//!     crash mid-write costs one line, not the history;
//!   * replaying the same events through the same EWMA gives the same numbers,
//!     so a reload is verifiable (`persist_and_reload_matches_the_live_value`).
//!
//! A line that does not parse is skipped and counted, never fatal: a truncated
//! final line is the normal cost of a power cut.

use std::collections::HashMap;
use std::io::Write;

use crate::json::{field_bool, field_f64, field_str, field_u64};
use crate::scoring::GatewayHistory;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// We reached the gateway and it answered. `rtt_ms` is the observed latency,
    /// `kbps` the throughput if we could estimate it (0 = unknown).
    Probe {
        gateway_id: String,
        rtt_ms: f64,
        kbps: f64,
    },
    /// The gateway did not answer at all.
    Unreachable { gateway_id: String },
    /// We paid and the gate granted access.
    Granted { gateway_id: String },
    /// The gate refused, with its own code, so the reason is auditable.
    Refused { gateway_id: String, code: String },
}

impl Event {
    pub fn gateway_id(&self) -> &str {
        match self {
            Event::Probe { gateway_id, .. }
            | Event::Unreachable { gateway_id }
            | Event::Granted { gateway_id }
            | Event::Refused { gateway_id, .. } => gateway_id,
        }
    }
}

pub struct Store {
    path: String,
    skipped: usize,
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

impl Event {
    pub fn to_json(&self, at: u64) -> String {
        let gw = escape(self.gateway_id());
        match self {
            Event::Probe { rtt_ms, kbps, .. } => format!(
                r#"{{"t":{at},"kind":"probe","gateway_id":"{gw}","rtt_ms":{rtt_ms:.3},"kbps":{kbps:.3}}}"#
            ),
            Event::Unreachable { .. } => {
                format!(r#"{{"t":{at},"kind":"unreachable","gateway_id":"{gw}"}}"#)
            }
            Event::Granted { .. } => {
                format!(r#"{{"t":{at},"kind":"granted","gateway_id":"{gw}"}}"#)
            }
            Event::Refused { code, .. } => format!(
                r#"{{"t":{at},"kind":"refused","gateway_id":"{gw}","code":"{}"}}"#,
                escape(code)
            ),
        }
    }

    pub fn from_json(line: &str) -> Option<Event> {
        let kind = field_str(line, "kind")?;
        let gateway_id = unescape(&field_str(line, "gateway_id")?);
        match kind.as_str() {
            "probe" => Some(Event::Probe {
                gateway_id,
                rtt_ms: field_f64(line, "rtt_ms")?,
                kbps: field_f64(line, "kbps").unwrap_or(0.0),
            }),
            "unreachable" => Some(Event::Unreachable { gateway_id }),
            "granted" => Some(Event::Granted { gateway_id }),
            "refused" => Some(Event::Refused {
                gateway_id,
                code: unescape(&field_str(line, "code").unwrap_or_default()),
            }),
            _ => None,
        }
    }
}

/// Fold one event into the histories. One function so that live recording and
/// replay can never disagree about what a failure means.
pub fn apply(histories: &mut HashMap<String, GatewayHistory>, event: &Event) {
    let entry = histories.entry(event.gateway_id().to_string()).or_default();
    match event {
        Event::Probe { rtt_ms, kbps, .. } => entry.record_success(*rtt_ms, 0.0, *kbps),
        Event::Unreachable { .. } => entry.record_failure(),
        Event::Granted { .. } => entry.record_success(entry.rtt_ms, entry.loss, entry.kbps),
        Event::Refused { .. } => entry.record_failure(),
    }
}

impl Store {
    pub fn open(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            skipped: 0,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Events that could not be parsed, for the caller to surface. Silently
    /// discarding history would make the scores quietly wrong.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    pub fn load(&mut self) -> HashMap<String, GatewayHistory> {
        let mut histories: HashMap<String, GatewayHistory> = HashMap::new();
        self.skipped = 0;
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return histories; // no history yet is not an error
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match Event::from_json(line) {
                Some(event) => apply(&mut histories, &event),
                None => self.skipped += 1,
            }
        }
        histories
    }

    pub fn record(&mut self, event: &Event) -> Result<(), String> {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = event.to_json(at);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("open {}: {e}", self.path))?;
        file.write_all(format!("{line}\n").as_bytes())
            .map_err(|e| format!("append {}: {e}", self.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn tmp(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("tgw-hist-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.jsonl").to_string_lossy().to_string()
    }

    #[test]
    fn events_round_trip_through_jsonl() {
        let path = tmp("roundtrip");
        let mut store = Store::open(path.clone());
        store
            .record(&Event::Probe {
                gateway_id: "http://10.0.0.1:2121/".into(),
                rtt_ms: 42.5,
                kbps: 900.0,
            })
            .unwrap();
        store
            .record(&Event::Refused {
                gateway_id: "http://10.0.0.1:2121/".into(),
                code: "payment-error-invalid-token".into(),
            })
            .unwrap();

        let histories = store.load();
        assert_eq!(store.skipped(), 0);
        let h = &histories["http://10.0.0.1:2121/"];
        assert_eq!((h.successes, h.failures), (1, 1));
        assert_eq!(h.rtt_ms, 42.5);
    }

    /// Persisting must not change the numbers: replay is the same fold.
    #[test]
    fn persist_and_reload_matches_the_live_value() {
        let path = tmp("ewma");
        let events = vec![
            Event::Probe {
                gateway_id: "g".into(),
                rtt_ms: 100.0,
                kbps: 500.0,
            },
            Event::Probe {
                gateway_id: "g".into(),
                rtt_ms: 200.0,
                kbps: 700.0,
            },
            Event::Unreachable {
                gateway_id: "g".into(),
            },
            Event::Probe {
                gateway_id: "g".into(),
                rtt_ms: 50.0,
                kbps: 100.0,
            },
        ];

        let mut live: HashMap<String, GatewayHistory> = HashMap::new();
        for e in &events {
            apply(&mut live, e);
        }

        let mut store = Store::open(path);
        for e in &events {
            store.record(e).unwrap();
        }
        let replayed = store.load();

        assert_eq!(live["g"].successes, replayed["g"].successes);
        assert_eq!(live["g"].failures, replayed["g"].failures);
        assert!((live["g"].rtt_ms - replayed["g"].rtt_ms).abs() < 1e-9);
        assert!((live["g"].kbps - replayed["g"].kbps).abs() < 1e-9);
    }

    /// A power cut leaves half a line. One bad line must not erase the history.
    #[test]
    fn a_truncated_final_line_is_skipped_and_counted() {
        let path = tmp("truncated");
        let mut store = Store::open(path.clone());
        store
            .record(&Event::Granted {
                gateway_id: "g".into(),
            })
            .unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(br#"{"t":1,"kind":"probe","gateway_id":"g","rtt_"#)
            .unwrap();
        drop(f);

        let histories = store.load();
        assert_eq!(store.skipped(), 1);
        assert_eq!(histories["g"].successes, 1);
    }

    #[test]
    fn an_identifier_with_a_quote_survives_a_round_trip() {
        let e = Event::Refused {
            gateway_id: "we\"ird".into(),
            code: "x\"y".into(),
        };
        let line = e.to_json(7);
        assert_eq!(Event::from_json(&line), Some(e));
        assert_eq!(json::field_u64(&line, "t"), Some(7));
    }
}
