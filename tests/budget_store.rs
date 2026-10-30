#![cfg(feature = "persistence")]
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use tollgate_client_core::budget_store::{BudgetStore, Error};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn path() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "budget-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&p).unwrap();
    p.join("ledger.sqlite")
}
#[test]
fn durable_full_face_plus_fee_and_idempotency() {
    let p = path();
    let mut s = BudgetStore::create(&p).unwrap();
    assert!(matches!(
        s.reserve("large", "sat", 1, 64, 0, 10),
        Err(Error::CapExceeded)
    ));
    s.reserve("one", "sat", 1, 8, 1, 10).unwrap();
    s.reserve("one", "sat", 1, 8, 1, 10).unwrap();
    assert!(s.reserve("one", "sat", 1, 7, 1, 10).is_err());
    drop(s);
    let mut s = BudgetStore::open(&p).unwrap();
    assert_eq!(s.usage("sat", 1).unwrap(), 9);
    s.settle("settle-one", "one", 2, 7).unwrap();
    s.settle("settle-one", "one", 2, 7).unwrap();
    assert_eq!(s.usage("sat", 2).unwrap(), 7);
    assert_eq!(s.usage("sat", 1).unwrap(), 0);
    assert!(s.settle("other", "one", 2, 7).is_err());
    assert!(s.release("release", "one", 2).is_err());
}
#[test]
fn all_prior_day_exposure_and_units() {
    let mut s = BudgetStore::create(path()).unwrap();
    s.reserve("old", "sat", 1, 8, 1, 10).unwrap();
    assert!(matches!(
        s.reserve("new", "sat", 2, 2, 0, 10),
        Err(Error::CapExceeded)
    ));
    s.reserve("msat", "msat", 2, 10, 0, 10).unwrap();
    s.release("cancel", "old", 2).unwrap();
    s.release("cancel", "old", 2).unwrap();
    s.reserve("new", "sat", 2, 10, 0, 10).unwrap();
    assert!(s.reserve("rollback", "sat", 1, 1, 0, 100).is_err());
}
#[test]
fn overflow_and_conflicting_operations_fail_closed() {
    let mut s = BudgetStore::create(path()).unwrap();
    assert!(s
        .reserve("overflow", "sat", 1, u64::MAX, 1, u64::MAX)
        .is_err());
    assert!(s
        .reserve("range", "sat", 1, i64::MAX as u64 + 1, 0, u64::MAX)
        .is_err());
    s.reserve("a", "sat", 1, 5, 0, 10).unwrap();
    assert!(s.settle("a", "a", 1, 5).is_err());
    assert!(s.settle("too-much", "a", 1, 6).is_err());
    assert_eq!(s.usage("sat", 1).unwrap(), 5);
    s.settle("done", "a", 1, 5).unwrap();
    assert!(s.reserve("done", "sat", 1, 1, 0, 10).is_err());
    assert!(s.settle("done", "a", 1, 4).is_err());
}
#[test]
fn corrupt_missing_unreadable_and_unknown_schema() {
    let p = path();
    assert!(BudgetStore::open(&p).is_err());
    std::fs::write(&p, b"not a database").unwrap();
    assert!(BudgetStore::open(&p).is_err());
    assert!(BudgetStore::create(&p).is_err());
    assert!(BudgetStore::open(p.parent().unwrap()).is_err());
    let p = path();
    drop(BudgetStore::create(&p).unwrap());
    let c = rusqlite::Connection::open(&p).unwrap();
    c.pragma_update(None, "user_version", 99).unwrap();
    drop(c);
    assert!(BudgetStore::open(&p).is_err());
}
#[test]
fn process_reservation_worker() {
    if let Ok(p) = std::env::var("BUDGET_CHILD_DB") {
        while !std::path::Path::new(&std::env::var("BUDGET_CHILD_GO").unwrap()).exists() {
            std::thread::yield_now();
        }
        let mut s = BudgetStore::open(p).unwrap();
        match s.reserve(
            &std::env::var("BUDGET_CHILD_ID").unwrap(),
            "sat",
            1,
            7,
            0,
            10,
        ) {
            Ok(()) => (),
            Err(Error::CapExceeded) => (),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
#[test]
fn two_processes_compete_for_last_budget() {
    let p = path();
    drop(BudgetStore::create(&p).unwrap());
    let go = p.with_extension("go");
    let mut children = vec![];
    for id in ["child-a", "child-b"] {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "process_reservation_worker", "--nocapture"])
                .env("BUDGET_CHILD_DB", &p)
                .env("BUDGET_CHILD_GO", &go)
                .env("BUDGET_CHILD_ID", id)
                .spawn()
                .unwrap(),
        );
    }
    std::fs::write(go, b"go").unwrap();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(BudgetStore::open(&p).unwrap().usage("sat", 1).unwrap(), 7);
}

#[test]
fn altered_schema_and_logical_corruption_are_rejected() {
    for sql in [
        "DROP INDEX one_terminal",
        "INSERT INTO operations VALUES('bad','a','release',1,9)",
        "DELETE FROM operations WHERE kind='reserve'",
        "UPDATE metadata SET high_day=0",
    ] {
        let p = path();
        let mut s = BudgetStore::create(&p).unwrap();
        s.reserve("a", "sat", 1, 9, 0, 10).unwrap();
        drop(s);
        rusqlite::Connection::open(&p)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        assert!(BudgetStore::open(&p).is_err(), "accepted {sql}");
    }
}

#[cfg(unix)]
#[test]
fn unreadable_permissions_fail_closed() {
    use std::os::unix::fs::PermissionsExt;
    let p = path();
    drop(BudgetStore::create(&p).unwrap());
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0)).unwrap();
    let failed = BudgetStore::open(&p).is_err();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(failed, "run tests as an unprivileged user");
}
