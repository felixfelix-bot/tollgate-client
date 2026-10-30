//! Real SIGKILL/reopen tests on Linux. Not power-loss or Android evidence.
//! Sync interception forwards successful real syscalls before notifying the parent.
#![cfg(all(feature = "persistence", target_os = "linux"))]
use std::os::unix::process::ExitStatusExt;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, OnceLock,
    },
    time::Duration,
};
use tollgate_client_core::budget_store::{BudgetStore, Error};
const LIMIT: Duration = Duration::from_secs(20);
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "budget-crash-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn db(&self) -> PathBuf {
        self.0.join("ledger.sqlite")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn shim() -> &'static Path {
    static SHIM: OnceLock<PathBuf> = OnceLock::new();
    SHIM.get_or_init(|| {
        let p = std::env::temp_dir().join(format!("budget-crash-sync-{}.so", std::process::id()));
        let status = Command::new("cc")
            .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/crash_sync.c"
            ))
            .args(["-ldl", "-o"])
            .arg(&p)
            .status()
            .unwrap();
        assert!(status.success(), "sync observer must compile");
        p
    })
}
struct Worker {
    child: Child,
    messages: mpsc::Receiver<String>,
}
impl Worker {
    fn start(db: &Path, mode: &str, sync_target: Option<&Path>) -> Self {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "crash_worker", "--nocapture"])
            .env("BUDGET_CRASH_DB", db)
            .env("BUDGET_CRASH_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(target) = sync_target {
            cmd.env("LD_PRELOAD", shim())
                .env("BUDGET_SYNC_TARGET", target);
        }
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, messages }
    }
    fn expect(&self, expected: &str) {
        let deadline = std::time::Instant::now() + LIMIT;
        loop {
            let line = self
                .messages
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or_else(|e| panic!("waiting for {expected}: {e}"));
            if line == expected {
                return;
            }
        }
    }
    fn go(&mut self) {
        self.child.stdin.as_mut().unwrap().write_all(b"g").unwrap();
    }
    fn kill(&mut self) {
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        assert_eq!(status.signal(), Some(9), "must die by SIGKILL, not unwind");
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn notify(msg: &str) {
    println!("{msg}");
    std::io::stdout().flush().unwrap();
}
fn wait() {
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
}
fn parked() {
    notify("CRASH_READY");
    wait();
    panic!("kill checkpoint released");
}
#[test]
fn crash_worker() {
    let Ok(db) = std::env::var("BUDGET_CRASH_DB") else {
        return;
    };
    let mode = std::env::var("BUDGET_CRASH_MODE").unwrap();
    if mode == "create" {
        let _store = BudgetStore::create(db).unwrap();
        parked();
        return;
    }
    if mode == "raw-transaction" {
        let c = rusqlite::Connection::open(db).unwrap();
        c.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=1; BEGIN IMMEDIATE;
            UPDATE metadata SET high_day=9;
            INSERT INTO reservations VALUES('uncommitted','sat',9,10,0,10,10);
            INSERT INTO operations VALUES('uncommitted','uncommitted','reserve',9,10);").unwrap();
        // Force dirty pages to the real database while retaining the transaction.
        // sqlite3_db_cacheflush is a real SQLite API, not a production test hook.
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_db_cacheflush(c.handle()) },
            rusqlite::ffi::SQLITE_OK
        );
        assert!(!c.is_autocommit());
        parked();
        return;
    }
    let mut s = BudgetStore::open(db).unwrap();
    match mode.as_str() {
        "reserve-before" | "settle-before" => parked(),
        "reserve" => {
            s.reserve("held", "sat", 1, 8, 1, 10).unwrap();
            parked();
        }
        "settle" => {
            s.settle("settled", "held", 2, 7).unwrap();
            parked();
        }
        "competitor" => {
            notify("OPENED");
            wait();
            assert!(matches!(
                s.reserve("competitor", "sat", 2, 2, 0, 10),
                Err(Error::CapExceeded)
            ));
            notify("DENIED");
            parked();
        }
        _ => panic!("unknown mode"),
    }
}
fn initialized(f: &Fixture, held: bool) {
    let mut s = BudgetStore::create(f.db()).unwrap();
    if held {
        s.reserve("held", "sat", 1, 8, 1, 10).unwrap();
    }
}
#[test]
fn kill_before_and_after_committed_reservation() {
    for (mode, expected) in [("reserve-before", 0), ("reserve", 9)] {
        let f = Fixture::new();
        initialized(&f, false);
        let mut w = Worker::start(&f.db(), mode, None);
        w.expect("CRASH_READY");
        w.kill();
        for _ in 0..3 {
            let mut s = BudgetStore::open(f.db()).unwrap();
            assert_eq!(s.usage("sat", 2).unwrap(), expected);
            if expected == 9 {
                s.reserve("held", "sat", 1, 8, 1, 10).unwrap();
                assert_eq!(s.usage("sat", 2).unwrap(), 9);
            }
        }
    }
}
#[test]
fn kill_before_and_after_committed_settlement_replay_no_double_debit() {
    for (mode, expected) in [("settle-before", 9), ("settle", 7)] {
        let f = Fixture::new();
        initialized(&f, true);
        let mut w = Worker::start(&f.db(), mode, None);
        w.expect("CRASH_READY");
        w.kill();
        assert_eq!(
            BudgetStore::open(f.db()).unwrap().usage("sat", 2).unwrap(),
            expected
        );
        for _ in 0..3 {
            let mut s = BudgetStore::open(f.db()).unwrap();
            s.settle("settled", "held", 2, 7).unwrap();
            assert_eq!(s.usage("sat", 2).unwrap(), 7);
            assert_eq!(s.usage("sat", 1).unwrap(), 0);
            assert!(s.settle("different", "held", 2, 7).is_err());
        }
    }
}
#[test]
fn kill_inside_real_reserve_and_settle_before_commit() {
    for (mode, held, expected) in [("reserve", false, 0), ("settle", true, 9)] {
        let f = Fixture::new();
        initialized(&f, held);
        // A rollback-journal sync occurs after SQL writes but before COMMIT completes.
        let journal = f.db().with_extension("sqlite-journal");
        let mut w = Worker::start(&f.db(), mode, Some(&journal));
        w.expect("CRASH_READY");
        assert!(std::fs::metadata(&journal).unwrap().len() > 0);
        w.kill();
        for _ in 0..3 {
            let s = BudgetStore::open(f.db()).unwrap();
            assert_eq!(s.usage("sat", 2).unwrap(), expected);
        }
        // Rolled-back high_day=2 must not reject day 1.
        if held {
            let mut s = BudgetStore::open(f.db()).unwrap();
            s.reserve("one-more", "sat", 1, 1, 0, 10).unwrap();
            assert_eq!(s.usage("sat", 1).unwrap(), 10);
        }
    }
}
#[test]
fn uncommitted_spilled_sqlite_transaction_rolls_back_after_kill() {
    let f = Fixture::new();
    initialized(&f, true);
    let mut w = Worker::start(&f.db(), "raw-transaction", None);
    w.expect("CRASH_READY");
    assert!(
        std::fs::metadata(f.db().with_extension("sqlite-journal"))
            .unwrap()
            .len()
            > 0
    );
    w.kill();
    for _ in 0..3 {
        let mut s = BudgetStore::open(f.db()).unwrap();
        assert_eq!(s.usage("sat", 1).unwrap(), 9);
        s.reserve("held", "sat", 1, 8, 1, 10).unwrap();
    }
    let mut s = BudgetStore::open(f.db()).unwrap();
    s.reserve("one-more", "sat", 1, 1, 0, 10).unwrap();
    assert_eq!(s.usage("sat", 1).unwrap(), 10);
}
#[test]
fn competing_open_process_cannot_over_admit_after_holder_killed() {
    let f = Fixture::new();
    initialized(&f, false);
    let mut competitor = Worker::start(&f.db(), "competitor", None);
    competitor.expect("OPENED");
    let mut holder = Worker::start(&f.db(), "reserve", None);
    holder.expect("CRASH_READY");
    holder.kill();
    competitor.go();
    competitor.expect("DENIED");
    competitor.expect("CRASH_READY");
    competitor.kill();
    assert_eq!(
        BudgetStore::open(f.db()).unwrap().usage("sat", 2).unwrap(),
        9
    );
}
#[test]
fn interrupted_real_initialization_fails_closed_without_reenrollment() {
    // First: actual create_new file sync. Second: schema transaction's journal sync.
    for journal in [false, true] {
        let f = Fixture::new();
        let target = if journal {
            f.db().with_extension("sqlite-journal")
        } else {
            f.db()
        };
        let mut w = Worker::start(&f.db(), "create", Some(&target));
        w.expect("CRASH_READY");
        w.kill();
        assert!(f.db().exists());
        for _ in 0..3 {
            assert!(BudgetStore::open(f.db()).is_err());
            assert!(BudgetStore::create(f.db()).is_err());
        }
    }
}
