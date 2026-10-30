//! Does the loop actually learn? Two runs against the same gate, one file.
//!
//! This is the requirement "track the reliability of tollgates, their price and
//! the quality of their internet connections" reduced to something falsifiable:
//! after a purchase, the next run must see a history entry, and a gate that does
//! not answer must accumulate failures.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use tollgate_client_core::agent::{AttemptResult, Journal, Wallet};
use tollgate_client_core::history::{Event, Store};
use tollgate_client_core::policy::Caps;
use tollgate_client_core::purchase::IntentState;
use tollgate_client_core::runner::run_recorded;

struct FakeWallet(String);
impl Wallet for FakeWallet {
    fn mint_token(&mut self, _m: &str, _u: &str, _a: u64) -> Result<String, String> {
        Ok(self.0.clone())
    }
    fn is_spent(&mut self, _t: &str) -> Result<bool, String> {
        Ok(false)
    }
}

#[derive(Default)]
struct MemJournal(Vec<(String, IntentState)>);
impl Journal for MemJournal {
    fn begin(&mut self, key: &str) -> Result<(), String> {
        self.0.push((key.to_string(), IntentState::Pending));
        Ok(())
    }
    fn set_state(&mut self, key: &str, state: IntentState) -> Result<(), String> {
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| k == key) {
            slot.1 = state;
        } else {
            self.0.push((key.to_string(), state));
        }
        Ok(())
    }
    fn state(&self, key: &str) -> Option<IntentState> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, s)| *s)
    }
}

const ADVERT: &str = r#"{"kind":10021,"tags":[["metric","milliseconds"],["step_size","60000"],["price_per_step","cashu","2","sat","https://mint.example","1"],["device-identifier","aa:bb:cc:dd:ee:ff"]]}"#;
const GRANTED: &str = r#"{"kind":1022,"tags":[["allotment","60000"],["metric","milliseconds"],["device-identifier","aa:bb:cc:dd:ee:ff"]]}"#;

/// Serves `requests` connections: GET advert, POST grant.
fn gate(requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for _ in 0..requests {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = if req.starts_with("POST") {
                GRANTED
            } else {
                ADVERT
            };
            let _ = sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
                .as_bytes(),
            );
            let _ = sock.flush();
        }
    });
    format!("http://{addr}/")
}

fn caps() -> Caps {
    Caps {
        preferred_unit: "sat".into(),
        max_cost_per_minute: 10.0,
        daily_budget: 100.0,
        spent_today: 0.0,
        switch_margin: 0.20,
    }
}

fn tmp_path(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("tgw-wire-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("history.jsonl").to_string_lossy().to_string()
}

#[test]
fn a_purchase_writes_a_probe_sample_and_a_grant() {
    let path = tmp_path("granted");
    let gate = gate(2);
    let mut store = Store::open(path.clone());
    let mut journal = MemJournal::default();

    let report = run_recorded(
        &gate,
        &caps(),
        &mut store,
        &mut FakeWallet("cashuBtoken".into()),
        &mut journal,
        "attempt-1",
        2000,
    )
    .unwrap();

    assert!(matches!(
        report.result,
        Some(AttemptResult::Finished { .. })
    ));
    assert!(report.probe_rtt_ms > 0.0, "the probe is timed");

    let events: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(events.len(), 2, "one probe + one grant: {events:?}");
    assert!(events[0].contains("\"kind\":\"probe\""));
    assert!(events[1].contains("\"kind\":\"granted\""));

    // And the next run sees it.
    let mut reader = Store::open(path);
    let histories = reader.load();
    let h = &histories[&gate];
    assert_eq!((h.successes, h.failures), (2, 0));
    assert!(h.rtt_ms > 0.0 && h.kbps > 0.0);
}

#[test]
fn an_unreachable_gate_is_recorded_as_a_failure_not_dropped() {
    let path = tmp_path("unreachable");
    // Nothing listening on this port.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        format!("http://{a}/")
    };
    let mut store = Store::open(path.clone());
    let mut journal = MemJournal::default();

    let err = run_recorded(
        &dead,
        &caps(),
        &mut store,
        &mut FakeWallet("cashuBtoken".into()),
        &mut journal,
        "attempt-2",
        1500,
    );
    assert!(err.is_err());

    let histories = Store::open(path).load();
    assert_eq!(
        histories[&dead].failures, 1,
        "an unreachable gate is evidence"
    );
}

#[test]
fn a_refused_purchase_counts_against_the_gate() {
    let path = tmp_path("refused");
    let mut store = Store::open(path.clone());
    let mut journal = MemJournal::default();

    // The wallet hands over a token; the gate answers a refusal (400 + notice).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for i in 0..2 {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let (code, body) = if i == 0 {
                (200, ADVERT.to_string())
            } else {
                (
                    400,
                    r#"{"kind":21023,"tags":[["code","payment-error-invalid-token"],["message","no"]]}"#
                        .to_string(),
                )
            };
            assert!(req.starts_with("GET") || req.starts_with("POST"));
            let _ = sock.write_all(
                format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
                .as_bytes(),
            );
        }
    });

    let gate = format!("http://{addr}/");
    let _ = run_recorded(
        &gate,
        &caps(),
        &mut store,
        &mut FakeWallet("cashuBtoken".into()),
        &mut journal,
        "attempt-4",
        2000,
    );

    let events: Vec<Event> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter_map(Event::from_json)
        .collect();
    assert!(
        events.iter().any(|e| matches!(e, Event::Refused { .. })),
        "a refusal must be recorded: {events:?}"
    );
}
