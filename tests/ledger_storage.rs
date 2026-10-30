#![cfg(all(feature = "persistence", target_os = "linux"))]
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use tollgate_client_core::budget_store::BudgetStore;
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "ledger-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
    fn db(&self) -> PathBuf {
        self.0.join("ledger")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn refuses_symlink_ledger_and_auxiliaries_without_touching_target() {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let f = Fixture::new();
        drop(BudgetStore::create(f.db()).unwrap());
        let target = f.0.join("target");
        fs::write(&target, b"untouched").unwrap();
        let candidate = f.0.join(format!("ledger{suffix}"));
        if suffix.is_empty() {
            fs::remove_file(&candidate).unwrap();
        }
        symlink(&target, &candidate).unwrap();
        assert!(BudgetStore::open(f.db()).is_err(), "accepted {suffix}");
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
        assert!(fs::symlink_metadata(candidate)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
#[test]
fn refuses_hardlinks_modes_and_nonregular_paths() {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        for kind in ["hardlink", "mode", "directory"] {
            let f = Fixture::new();
            drop(BudgetStore::create(f.db()).unwrap());
            let candidate = f.0.join(format!("ledger{suffix}"));
            if suffix.is_empty() {
                fs::remove_file(&candidate).unwrap();
            }
            match kind {
                "hardlink" => {
                    fs::hard_link(
                        {
                            let p = f.0.join("target");
                            fs::write(&p, b"x").unwrap();
                            p
                        },
                        &candidate,
                    )
                    .unwrap();
                }
                "mode" => {
                    fs::write(&candidate, b"").unwrap();
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o666)).unwrap();
                }
                _ => fs::create_dir(&candidate).unwrap(),
            }
            assert!(
                BudgetStore::open(f.db()).is_err(),
                "accepted {suffix} {kind}"
            );
        }
    }
}
#[test]
fn rejects_unsafe_parent_before_creation_and_open() {
    let f = Fixture::new();
    drop(BudgetStore::create(f.db()).unwrap());
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(BudgetStore::open(f.db()).is_err());
    assert!(BudgetStore::create(f.0.join("new")).is_err());
    assert!(!f.0.join("new").exists());
}
#[test]
fn parent_replacement_race_under_untrusted_ancestor_is_refused() {
    let f = Fixture::new();
    let parent = f.0.join("parent");
    let other = f.0.join("other");
    fs::create_dir(&parent).unwrap();
    drop(BudgetStore::create(parent.join("ledger")).unwrap());
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o777)).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..500 {
                fs::rename(&parent, &other).unwrap();
                fs::rename(&other, &parent).unwrap();
            }
        });
        for _ in 0..500 {
            assert!(BudgetStore::open(parent.join("ledger")).is_err());
        }
    });
}
#[test]
fn enrollment_refuses_auxiliary_before_creating_database() {
    let f = Fixture::new();
    symlink("absent", f.0.join("ledger-journal")).unwrap();
    assert!(BudgetStore::create(f.db()).is_err());
    assert!(!f.db().exists());
}

#[test]
fn sqlite_nofollow_rejects_main_and_ancestor_symlinks() {
    use rusqlite::{Connection, OpenFlags};
    let f = Fixture::new();
    drop(BudgetStore::create(f.db()).unwrap());
    let alias = f.0.join("alias");
    symlink(f.db(), &alias).unwrap();
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    assert!(Connection::open_with_flags_and_vfs(&alias, flags, "unix").is_err());
    let directory_alias = f.0.join("directory-alias");
    symlink(&f.0, &directory_alias).unwrap();
    assert!(
        Connection::open_with_flags_and_vfs(directory_alias.join("ledger"), flags, "unix").is_err()
    );
    assert!(BudgetStore::open(directory_alias.join("ledger")).is_err());
    assert!(BudgetStore::open(f.0.join("../ledger")).is_err());
}
#[test]
fn existing_recovery_file_blocks_enrollment_and_is_preserved() {
    let f = Fixture::new();
    let recovery = f.0.join("ledger-wal");
    fs::write(&recovery, b"stranded recovery").unwrap();
    fs::set_permissions(&recovery, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(BudgetStore::create(f.db()).is_err());
    assert!(!f.db().exists());
    assert_eq!(fs::read(recovery).unwrap(), b"stranded recovery");
}
