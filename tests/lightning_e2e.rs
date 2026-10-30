//! The Lightning lane over real sockets: request an invoice, pay it, poll.
//!
//! The gate is faked; the client, the JSON reading and the state machine are real.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use tollgate_client_core::lightning::{buy, LightningError, LightningPayer};

struct FakePayer {
    paid: Arc<Mutex<Vec<(String, u64)>>>,
}

impl LightningPayer for FakePayer {
    fn pay(&mut self, bolt11: &str, amount_sat: u64) -> Result<String, String> {
        self.paid.lock().unwrap().push((bolt11.to_string(), amount_sat));
        Ok("manual".into())
    }
}

const INVOICE: &str = r#"{"status":1,"quote":"q_1","invoice":"lnbc20n1example","mint_url":"https://mint.example","amount":2,"expiry":1893456000,"state":"UNPAID","access_granted":false}"#;
const PENDING: &str = r#"{"status":1,"quote":"q_1","state":"PENDING","access_granted":false}"#;
const GRANTED: &str = r#"{"status":1,"quote":"q_1","state":"PAID","access_granted":true,"allotment":60000,"metric":"milliseconds"}"#;
const GRANT_FAILED: &str = r#"{"status":0,"error":"payment received, do not pay again","code":"access-grant-failed"}"#;
const TABLE_FULL: &str = r#"{"status":0,"error":"too many unpaid invoices","code":"quote-table-full","retry_after":60}"#;

fn fake_gate(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    thread::spawn(move || {
        for (status, body) in replies {
            let Ok((mut sock, _)) = listener.accept() else { return };
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            seen2.lock().unwrap().push(req.lines().next().unwrap_or("").to_string());
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.flush();
        }
    });
    (format!("http://{addr}"), seen)
}

#[test]
fn requests_pays_and_polls_until_the_gate_opens() {
    let (gate, seen) = fake_gate(vec![
        (200, INVOICE.to_string()),
        (200, PENDING.to_string()),
        (200, PENDING.to_string()),
        (200, GRANTED.to_string()),
    ]);
    let paid = Arc::new(Mutex::new(Vec::new()));
    let mut payer = FakePayer { paid: paid.clone() };

    let status = buy(&gate, 2, "https://mint.example", &mut payer, 2000, 0, 10).unwrap();

    assert!(status.access_granted);
    assert_eq!(status.allotment, Some(60_000));
    assert_eq!(status.metric.as_deref(), Some("milliseconds"));
    let paid = paid.lock().unwrap();
    assert_eq!(paid.len(), 1, "the invoice must be paid exactly once");
    assert_eq!(paid[0].0, "lnbc20n1example");
    assert_eq!(paid[0].1, 2);
    // One POST to create, then GET polls: never a second POST.
    let posts = seen.lock().unwrap().iter().filter(|l| l.starts_with("POST")).count();
    assert_eq!(posts, 1);
}

/// A 503 `access-grant-failed` means "paid, gate not open yet". It must never be
/// read as "payment failed" — re-paying is exactly the bug.
#[test]
fn a_grant_failure_keeps_polling_and_never_pays_twice() {
    let (gate, seen) = fake_gate(vec![
        (200, INVOICE.to_string()),
        (503, GRANT_FAILED.to_string()),
        (200, GRANTED.to_string()),
    ]);
    let paid = Arc::new(Mutex::new(Vec::new()));
    let mut payer = FakePayer { paid: paid.clone() };

    let status = buy(&gate, 2, "https://mint.example", &mut payer, 2000, 0, 10).unwrap();
    assert!(status.access_granted);
    assert_eq!(paid.lock().unwrap().len(), 1);
    assert_eq!(seen.lock().unwrap().iter().filter(|l| l.starts_with("POST")).count(), 1);
}

#[test]
fn a_full_quote_table_is_a_refusal_with_its_retry_hint() {
    let (gate, _seen) = fake_gate(vec![(429, TABLE_FULL.to_string())]);
    let paid = Arc::new(Mutex::new(Vec::new()));
    let mut payer = FakePayer { paid: paid.clone() };

    match buy(&gate, 2, "https://mint.example", &mut payer, 2000, 0, 3) {
        Err(LightningError::Refused { code, retry_after, .. }) => {
            assert_eq!(code, "quote-table-full");
            assert_eq!(retry_after, Some(60));
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    assert!(paid.lock().unwrap().is_empty(), "nothing to pay without an invoice");
}

#[test]
fn exhausting_the_polls_says_paid_but_not_granted_rather_than_failing_the_payment() {
    let (gate, _seen) = fake_gate(vec![
        (200, INVOICE.to_string()),
        (200, PENDING.to_string()),
        (200, PENDING.to_string()),
    ]);
    let paid = Arc::new(Mutex::new(Vec::new()));
    let mut payer = FakePayer { paid: paid.clone() };

    match buy(&gate, 2, "https://mint.example", &mut payer, 2000, 0, 2) {
        Err(LightningError::PaidButNotGranted { .. }) => {}
        other => panic!("expected PaidButNotGranted, got {other:?}"),
    }
    assert_eq!(paid.lock().unwrap().len(), 1);
}
