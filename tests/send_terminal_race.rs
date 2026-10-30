#![cfg(feature = "persistence")]
use std::sync::{Arc, Barrier};
use tollgate_client_core::{budget_store::BudgetStore, send_state::*};

fn race(settle: bool) {
    const N: usize = 2000;
    let dir = std::env::temp_dir().join(format!(
        "send-terminal-stress-{}-{settle}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("ledger");
    let mut sender = BudgetStore::create(&path).unwrap();
    let mut terminal = BudgetStore::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let b = barrier.clone();
    let worker = std::thread::spawn(move || {
        let mut accepted = 0;
        for i in 0..N {
            b.wait();
            let result = if settle {
                terminal.settle(&format!("end-{i}"), &format!("op-{i}"), 1, 0)
            } else {
                terminal.release(&format!("end-{i}"), &format!("op-{i}"), 1)
            };
            accepted += usize::from(result.is_ok());
            b.wait();
        }
        accepted
    });
    let mut posts = 0;
    for i in 0..N {
        let plan = TrustedSendPlan {
            operation: format!("op-{i}"),
            network: "net".into(),
            vault: "vault".into(),
            mint: "https://mint.example".into(),
            unit: "sat".into(),
            day: 1,
            face: 1,
            fee: 0,
            cap: N as u64,
            proofs: vec![TrustedProofIdentity {
                y: format!("y-{i}"),
                keyset: "key".into(),
                amount: 1,
            }],
        };
        barrier.wait();
        let result = sender.send_once_nonproduction(&plan, "attempt", |_| {
            posts += 1;
            DeliveryObservation::Unknown
        });
        barrier.wait();
        result.unwrap();
    }
    let accepted = worker.join().unwrap();
    let usage = sender.usage("sat", 2).unwrap();
    drop(sender);
    let reopened = BudgetStore::open(&path);
    eprintln!(
        "settle={settle} bypasses={accepted} posts={posts} usage={usage} reopen_ok={}",
        reopened.is_ok()
    );
    assert_eq!(accepted, 0);
    assert_eq!(posts, N);
    assert_eq!(usage, N as u64);
    assert_eq!(reopened.unwrap().usage("sat", 2).unwrap(), N as u64);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn public_send_release_stress() {
    race(false);
}
#[test]
fn public_send_settle_stress() {
    race(true);
}
