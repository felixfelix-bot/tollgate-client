#![cfg(all(feature = "persistence", feature = "nonproduction-test-harness"))]
use std::sync::atomic::{AtomicUsize, Ordering};
use tollgate_client_core::budget_store::BudgetStore;
use tollgate_client_core::send_state::*;
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn path() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "send-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&p).unwrap();
    p.join("ledger")
}
fn plan() -> TrustedSendPlan {
    TrustedSendPlan {
        operation: "entitlement-1".into(),
        network: "network-gate-generation".into(),
        vault: "protected-vault-reference".into(),
        mint: "https://mint.example".into(),
        unit: "sat".into(),
        day: 1,
        face: 8,
        fee: 1,
        cap: 10,
        proofs: vec![TrustedProofIdentity {
            y: "public-y".into(),
            keyset: "keyset".into(),
            amount: 8,
        }],
    }
}
#[test]
fn repeat_attempts_and_overlap_never_resend() {
    let p = path();
    let mut s = BudgetStore::create(&p).unwrap();
    let mut posts = 0;
    let a = s
        .send_once_nonproduction(&plan(), "tick-1", |_| {
            posts += 1;
            DeliveryObservation::Unknown
        })
        .unwrap();
    assert_eq!(a.delivery, DeliveryObservation::Unknown);
    s.send_once_nonproduction(&plan(), "tick-2", |_| {
        posts += 1;
        DeliveryObservation::GrantNotice
    })
    .unwrap();
    assert_eq!(posts, 1);
    let mut other = plan();
    other.operation = "other".into();
    assert!(s
        .send_once_nonproduction(&other, "tick-3", |_| panic!("overlap POST"))
        .is_err());
    assert_eq!(s.usage("sat", 2).unwrap(), 9);
    assert!(s.release("release", &plan().operation, 1).is_err());
    assert!(s.settle("settle", &plan().operation, 1, 8).is_err());
}
#[test]
fn interrupted_after_mark_reopen_is_status_only() {
    let p = path();
    let mut s = BudgetStore::create(&p).unwrap();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.send_once_nonproduction(&plan(), "a", |_| panic!("interrupted before bytes"))
    }));
    drop(s);
    let mut s = BudgetStore::open(&p).unwrap();
    let status = s
        .send_once_nonproduction(&plan(), "b", |_| panic!("second POST"))
        .unwrap();
    assert_eq!(status.delivery, DeliveryObservation::Unknown);
    for observation in [
        ProofObservation::AllUnspent,
        ProofObservation::PendingOrMixed,
        ProofObservation::AllSpent,
    ] {
        s.observe_proofs(&plan().operation, observation).unwrap();
        let status = s
            .send_once_nonproduction(&plan(), "c", |_| panic!("observation permits POST"))
            .unwrap();
        assert!(!status.internet_verified);
        assert_eq!(status.proofs, observation);
        assert_eq!(s.usage("sat", 2).unwrap(), 9);
    }
}
#[test]
fn persistence_failure_zero_post_and_atomic_claim() {
    let p = path();
    let mut s = BudgetStore::create(&p).unwrap();
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch("CREATE TRIGGER deny_send BEFORE INSERT ON sends BEGIN SELECT RAISE(ABORT,'injected persistence failure'); END;").unwrap();
    assert!(s
        .send_once_nonproduction(&plan(), "a", |_| panic!("POST after write failure"))
        .is_err());
    assert_eq!(s.usage("sat", 1).unwrap(), 0);
    c.execute_batch("DROP TRIGGER deny_send").unwrap();
    s.send_once_nonproduction(&plan(), "b", |_| DeliveryObservation::RefusalOrMalformed)
        .unwrap();
    assert_eq!(s.usage("sat", 2).unwrap(), 9);
}
#[test]
fn send_worker() {
    if let Ok(p) = std::env::var("SEND_DB") {
        let mut s = BudgetStore::open(&p).unwrap();
        s.send_once_nonproduction(&plan(), &std::process::id().to_string(), |_| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(format!("{p}.posts"))
                .unwrap();
            writeln!(f, "POST").unwrap();
            DeliveryObservation::GrantNotice
        })
        .unwrap();
    }
}
#[test]
fn two_processes_one_owner() {
    let p = path();
    drop(BudgetStore::create(&p).unwrap());
    let log = format!("{}.posts", p.display());
    std::fs::write(&log, "").unwrap();
    let mut workers = vec![];
    for _ in 0..2 {
        workers.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "send_worker"])
                .env("SEND_DB", &p)
                .spawn()
                .unwrap(),
        );
    }
    for mut w in workers {
        assert!(w.wait().unwrap().success());
    }
    assert_eq!(std::fs::read_to_string(log).unwrap(), "POST\n");
    assert_eq!(BudgetStore::open(p).unwrap().usage("sat", 2).unwrap(), 9);
}
#[test]
fn invalid_changed_and_old_schema_fail_closed() {
    let p = path();
    let mut s = BudgetStore::create(&p).unwrap();
    let mut bad = plan();
    bad.proofs.push(bad.proofs[0].clone());
    assert!(s.send_once_nonproduction(&bad, "a", |_| panic!()).is_err());
    bad = plan();
    bad.face = 7;
    assert!(s.send_once_nonproduction(&bad, "a", |_| panic!()).is_err());
    s.send_once_nonproduction(&plan(), "a", |_| DeliveryObservation::GrantNotice)
        .unwrap();
    bad = plan();
    bad.network = "different".into();
    assert!(s.send_once_nonproduction(&bad, "a", |_| panic!()).is_err());
    drop(s);
    rusqlite::Connection::open(&p)
        .unwrap()
        .pragma_update(None, "user_version", 1)
        .unwrap();
    assert!(BudgetStore::open(&p).is_err());
}

#[test]
fn partial_overlap_reordered_proofs_and_accounting_replay() {
    let p = path();
    let mut s = BudgetStore::create(&p).unwrap();
    let mut a = plan();
    a.proofs[0].amount = 4;
    a.proofs.push(TrustedProofIdentity {
        y: "second-y".into(),
        keyset: "keyset".into(),
        amount: 4,
    });
    s.send_once_nonproduction(&a, "first", |_| DeliveryObservation::Unknown)
        .unwrap();
    a.proofs.reverse();
    s.send_once_nonproduction(&a, "next", |_| panic!("reordered POST"))
        .unwrap();
    a.operation = "subset".into();
    a.proofs.pop();
    a.face = 4;
    a.cap = 100;
    assert!(s
        .send_once_nonproduction(&a, "other", |_| panic!("subset POST"))
        .is_err());
    assert_eq!(s.usage("sat", 1).unwrap(), 9);
    s.reserve("accounting", "sat", 1, 8, 1, 100).unwrap();
    s.release("cancel", "accounting", 1).unwrap();
    let mut b = plan();
    b.operation = "accounting".into();
    b.cap = 100;
    b.proofs[0].y = "fresh-y".into();
    assert!(s
        .send_once_nonproduction(&b, "attempt", |_| panic!("reservation replay POST"))
        .is_err());
}
#[test]
fn response_persistence_failure_keeps_durable_ambiguity() {
    let p = path();
    let mut s = BudgetStore::create(&p).unwrap();
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch("CREATE TRIGGER deny_response BEFORE UPDATE ON sends BEGIN SELECT RAISE(ABORT,'response persistence failure'); END;").unwrap();
    let mut posts = 0;
    assert!(s
        .send_once_nonproduction(&plan(), "a", |_| {
            posts += 1;
            DeliveryObservation::GrantNotice
        })
        .is_err());
    assert_eq!(posts, 1);
    c.execute_batch("DROP TRIGGER deny_response").unwrap();
    drop(c);
    drop(s);
    let mut s = BudgetStore::open(&p).unwrap();
    assert_eq!(
        s.send_once_nonproduction(&plan(), "b", |_| panic!("resend"))
            .unwrap()
            .delivery,
        DeliveryObservation::Unknown
    );
    assert_eq!(s.usage("sat", 2).unwrap(), 9);
}
