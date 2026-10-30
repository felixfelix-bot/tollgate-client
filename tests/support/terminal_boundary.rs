use super::*;
use crate::send_state::*;
use std::sync::mpsc;

thread_local! {
    static BUSY_SYNC: std::cell::RefCell<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = const { std::cell::RefCell::new(None) };
}
fn wait_for_sender(_: i32) -> bool {
    BUSY_SYNC.with(|slot| {
        let slot = slot.borrow();
        let (entered, resume) = slot.as_ref().unwrap();
        entered.send(()).unwrap();
        resume.recv_timeout(Duration::from_secs(10)).unwrap();
    });
    true
}

fn terminal_waiting_for_writer(settle: bool) {
    let dir =
        std::env::temp_dir().join(format!("terminal-boundary-{}-{settle}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("ledger");
    let mut sender = BudgetStore::create(&path).unwrap();
    let mut terminal = BudgetStore::open(&path).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    // RESERVED allows the old pre-transaction SELECT but blocks BEGIN.
    // No fabricated rows: the competing send uses the public API.
    sender.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let worker = std::thread::spawn(move || {
        BUSY_SYNC.with(|s| *s.borrow_mut() = Some((entered_tx, resume_rx)));
        terminal.db.busy_handler(Some(wait_for_sender)).unwrap();
        if settle {
            terminal.settle("terminal", "send", 1, 0)
        } else {
            terminal.release("terminal", "send", 1)
        }
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    sender.db.execute_batch("ROLLBACK").unwrap();
    let plan = TrustedSendPlan {
        operation: "send".into(),
        network: "net".into(),
        vault: "vault".into(),
        mint: "https://mint.example".into(),
        unit: "sat".into(),
        day: 1,
        face: 8,
        fee: 1,
        cap: 10,
        proofs: vec![TrustedProofIdentity {
            y: "y".into(),
            keyset: "key".into(),
            amount: 8,
        }],
    };
    let mut calls = 0;
    sender
        .send_once(&plan, "attempt", |_| {
            calls += 1;
            DeliveryObservation::Unknown
        })
        .unwrap();
    resume_tx.send(()).unwrap();
    let result = worker.join().unwrap();
    let usage = sender.usage("sat", 2).unwrap();
    drop(sender);
    let reopened = BudgetStore::open(&path);
    eprintln!(
        "settle={settle} terminal={result:?} calls={calls} usage={usage} reopen_ok={}",
        reopened.is_ok()
    );
    assert!(matches!(result, Err(Error::Conflict)));
    assert_eq!(calls, 1);
    assert_eq!(usage, 9);
    let mut reopened = reopened.unwrap();
    assert_eq!(reopened.usage("sat", 2).unwrap(), 9);
    reopened
        .send_once(&plan, "replay", |_| panic!("resend"))
        .unwrap();
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn release_waiting_for_send_transaction() {
    terminal_waiting_for_writer(false);
}
#[test]
fn settle_waiting_for_send_transaction() {
    terminal_waiting_for_writer(true);
}
