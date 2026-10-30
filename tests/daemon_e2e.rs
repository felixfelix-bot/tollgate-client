//! End-to-end over a real socket: the shipped HTTP client, the real advert parser,
//! the real policy, the real purchase classifier, and a journal on disk.
//!
//! The gate is faked with a TcpListener because that is the only part we do not
//! control; everything between the socket and the journal is production code.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use tollgate_client_core::agent::{AttemptResult, Journal};
use tollgate_client_core::policy::Caps;
use tollgate_client_core::purchase::{intent_key, IntentState};
use tollgate_client_core::runner::{run_once, FileJournal, FileWallet};

const ADVERT: &str = r#"{"kind":10021,"tags":[["metric","milliseconds"],["step_size","60000"],["tips","1","2"],["price_per_step","cashu","2","sat","https://mint.example","1"]]}"#;

fn caps() -> Caps {
    Caps {
        preferred_unit: "sat".to_string(),
        max_cost_per_minute: 10.0,
        daily_budget: 100.0,
        spent_today: 0.0,
        switch_margin: 0.20,
    }
}

/// Serve `replies` in order; the `nth` request gets `replies[nth]`.
fn fake_gate(replies: Vec<(u16, String)>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for (status, body) in replies {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let first = req.lines().next().unwrap_or("").to_string();
            let _ = tx.send(first);
            let reason = if status == 200 { "OK" } else { "Bad Request" };
            let resp = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.flush();
        }
    });
    (format!("http://{addr}/"), rx)
}

#[test]
fn probes_decides_buys_and_journals_over_a_real_socket() {
    let (url, rx) = fake_gate(vec![
        (200, ADVERT.to_string()),
        (200, r#"{"kind":1022,"tags":[["allotment","60000"],["metric","milliseconds"],["device-identifier","aa:bb"]]}"#.to_string()),
    ]);
    let dir = std::env::temp_dir().join(format!("tg-agent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let journal_path = dir.join("journal.txt");

    let mut wallet = FileWallet {
        token: "cashuBpreminted".into(),
    };
    let mut journal = FileJournal::open(&journal_path).unwrap();
    let report = run_once(
        &url,
        &caps(),
        &HashMap::new(),
        &mut wallet,
        &mut journal,
        "attempt-1",
        2000,
    )
    .unwrap();

    assert_eq!(report.advert_offers, 1);
    match report.result {
        Some(AttemptResult::Finished { state, cost, .. }) => {
            assert_eq!(state, IntentState::Granted);
            assert!((cost - 2.0).abs() < 1e-9);
        }
        other => panic!("expected Finished/Granted, got {other:?}"),
    }
    // The gate really was asked for the advert and then paid.
    assert!(rx.recv().unwrap().starts_with("GET /"));
    assert!(rx.recv().unwrap().starts_with("POST /"));

    // The journal survives the process: re-open it and the grant is still there.
    let reopened = FileJournal::open(&journal_path).unwrap();
    assert_eq!(
        reopened.state(&intent_key("attempt-1")),
        Some(IntentState::Granted)
    );
}

#[test]
fn a_second_run_with_the_same_attempt_does_not_touch_the_gate_again() {
    let (url, _rx) = fake_gate(vec![(200, ADVERT.to_string())]);
    let dir = std::env::temp_dir().join(format!("tg-agent-dup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let journal_path = dir.join("journal.txt");

    let mut wallet = FileWallet {
        token: "cashuBpreminted".into(),
    };
    let mut journal = FileJournal::open(&journal_path).unwrap();
    journal.begin(&intent_key("attempt-1")).unwrap();
    journal
        .set_state(&intent_key("attempt-1"), IntentState::Granted)
        .unwrap();

    let report = run_once(
        &url,
        &caps(),
        &HashMap::new(),
        &mut wallet,
        &mut journal,
        "attempt-1",
        2000,
    )
    .unwrap();
    assert!(
        matches!(report.result, Some(AttemptResult::AlreadyAttempted { .. })),
        "got {:?}",
        report.result
    );
}

#[test]
fn a_definitive_refusal_is_journalled_and_not_retried() {
    let (url, _rx) = fake_gate(vec![
        (200, ADVERT.to_string()),
        (400, r#"{"kind":21023,"tags":[["code","payment-error-invalid-token"],["message","bad token"]]}"#.to_string()),
    ]);
    let dir = std::env::temp_dir().join(format!("tg-agent-ref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let journal_path = dir.join("journal.txt");

    let mut wallet = FileWallet {
        token: "cashuBpreminted".into(),
    };
    let mut journal = FileJournal::open(&journal_path).unwrap();
    let report = run_once(
        &url,
        &caps(),
        &HashMap::new(),
        &mut wallet,
        &mut journal,
        "attempt-2",
        2000,
    )
    .unwrap();
    match report.result {
        Some(AttemptResult::Finished { state, .. }) => assert_eq!(state, IntentState::Refused),
        other => panic!("expected Finished/Refused, got {other:?}"),
    }
    assert_eq!(
        FileJournal::open(&journal_path)
            .unwrap()
            .state(&intent_key("attempt-2")),
        Some(IntentState::Refused)
    );
}

#[test]
fn an_offer_over_the_cap_costs_a_probe_and_nothing_else() {
    let (url, rx) = fake_gate(vec![(200, ADVERT.to_string())]);
    let dir = std::env::temp_dir().join(format!("tg-agent-cap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut c = caps();
    c.max_cost_per_minute = 0.5; // the advert asks 2.0/min

    let mut wallet = FileWallet {
        token: "cashuBpreminted".into(),
    };
    let mut journal = FileJournal::open(dir.join("journal.txt")).unwrap();
    let report = run_once(
        &url,
        &c,
        &HashMap::new(),
        &mut wallet,
        &mut journal,
        "attempt-3",
        2000,
    )
    .unwrap();

    assert!(
        report.result.is_none(),
        "nothing should be attempted: {:?}",
        report.result
    );
    assert!(rx.recv().unwrap().starts_with("GET /"));
    assert!(
        rx.recv().is_err(),
        "the gate must not be paid when a cap blocks the buy"
    );
}
