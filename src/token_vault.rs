//! Offline immutable, SHA-256-addressed bearer-byte storage.
//!
//! Linux/Android only: directory-relative syscalls pin every traversed directory.
//! Existing ancestors must be owned by root or the effective user and not writable
//! by group/other (root-owned sticky directories are allowed). The final directory
//! must be owned by the effective user, mode 0700; files must be 0600, single-link
//! regular files. No symlinks or `..` components are accepted.
//!
//! This protects against other UIDs, not root or a malicious process with the same
//! UID. It does not encrypt, erase memory, or defend against disk rollback. Never
//! print returned bearer bytes. Failed/interrupted writes can leave a partial
//! object; verification then fails closed rather than repairing/overwriting it.
use cdk::secp256k1::hashes::{sha256, Hash};
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::raw::{c_char, c_int, c_uint};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

#[cfg(not(any(
    target_arch = "x86",
    target_arch = "x86_64",
    target_arch = "arm",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
compile_error!("token_vault syscall flags require an audited target architecture");

// Linux UAPI flags; ARM uses its historical flag layout. This module is gated
// to Linux/Android architectures whose flag layout is specified here.
const O_CLOEXEC: c_int = 0o2000000;
const O_CREAT: c_int = 0o100;
const O_EXCL: c_int = 0o200;
const O_NONBLOCK: c_int = 0o4000;
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
const O_DIRECTORY: c_int = 0o40000;
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
const O_NOFOLLOW: c_int = 0o100000;
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
const O_DIRECTORY: c_int = 0o200000;
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
const O_NOFOLLOW: c_int = 0o400000;
extern "C" {
    fn openat(fd: c_int, name: *const c_char, flags: c_int, ...) -> c_int;
    fn mkdirat(fd: c_int, name: *const c_char, mode: c_uint) -> c_int;
    fn geteuid() -> c_uint;
}

/// Opaque SHA-256 identity, never a path or bearer. Intentionally no Debug.
#[derive(Clone, PartialEq, Eq)]
pub struct VaultRef([u8; 32]);
impl VaultRef {
    /// Persist this fixed-size identifier alongside application state.
    pub fn digest(&self) -> [u8; 32] {
        self.0
    }
    /// Restore an identifier; load still verifies the complete stored content.
    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }
    fn name(&self) -> CString {
        let hex: String = self.0.iter().map(|b| format!("{b:02x}")).collect();
        CString::new(hex).expect("hex has no NUL")
    }
}

/// Open directory capability; not a mutable pathname. Intentionally no Debug.
pub struct ProtectedTokenVault {
    directory: File,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn uid() -> u32 {
    // SAFETY: geteuid has no pointer arguments or preconditions.
    unsafe { geteuid() }
}
fn open_relative(parent: &File, name: &CString, flags: c_int, mode: c_uint) -> io::Result<File> {
    // SAFETY: parent remains open, name is NUL-terminated, and mode is supplied
    // for the variadic argument (used only with O_CREAT). Returned fd is owned.
    let fd = unsafe {
        openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | O_CLOEXEC | O_NOFOLLOW,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a new owned fd.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn validate_dir(meta: &Metadata, private: bool) -> io::Result<()> {
    if !meta.is_dir() {
        return Err(invalid("vault path is not a directory"));
    }
    let mode = meta.mode() & 0o7777;
    if private {
        if meta.uid() != uid() || mode != 0o700 {
            return Err(invalid("vault directory ownership or permissions invalid"));
        }
    } else {
        let trusted_owner = meta.uid() == 0 || meta.uid() == uid();
        let trusted_sticky = meta.uid() == 0 && mode & 0o1000 != 0;
        if !trusted_owner || (mode & 0o022 != 0 && !trusted_sticky) {
            return Err(invalid("unsafe vault ancestor"));
        }
    }
    Ok(())
}
fn validate_file(meta: &Metadata) -> io::Result<()> {
    if !meta.is_file() || meta.uid() != uid() || meta.mode() & 0o7777 != 0o600 || meta.nlink() != 1
    {
        return Err(invalid(
            "vault object type, ownership, links or permissions invalid",
        ));
    }
    Ok(())
}
impl ProtectedTokenVault {
    /// Open or create only the final directory. Ancestors must already exist.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let input = path.as_ref();
        if input.as_os_str().is_empty() {
            return Err(invalid("empty vault path"));
        }
        let absolute = if input.is_absolute() {
            input.to_path_buf()
        } else {
            std::env::current_dir()?.join(input)
        };
        let mut names = Vec::new();
        for part in absolute.components() {
            match part {
                Component::RootDir | Component::CurDir => (),
                Component::Normal(name) => names
                    .push(CString::new(name.as_bytes()).map_err(|_| invalid("NUL in vault path"))?),
                _ => return Err(invalid("parent traversal in vault path")),
            }
        }
        if names.is_empty() {
            return Err(invalid("vault must not be filesystem root"));
        }
        let mut dir = File::open("/")?;
        validate_dir(&dir.metadata()?, false)?;
        for (index, name) in names.iter().enumerate() {
            let last = index + 1 == names.len();
            let next = match open_relative(&dir, name, O_DIRECTORY, 0) {
                Ok(file) => file,
                Err(error) if last && error.kind() == io::ErrorKind::NotFound => {
                    // SAFETY: valid parent fd and NUL-terminated component.
                    let result = unsafe { mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
                    if result != 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    let file = open_relative(&dir, name, O_DIRECTORY, 0)?;
                    validate_dir(&file.metadata()?, true)?;
                    file.sync_all()?;
                    dir.sync_all()?;
                    file
                }
                Err(error) => return Err(error),
            };
            validate_dir(&next.metadata()?, last)?;
            if last {
                // Also covers a concurrent creator whose mkdir became visible
                // before it synced the parent directory.
                next.sync_all()?;
                dir.sync_all()?;
            }
            dir = next;
        }
        Ok(Self { directory: dir })
    }

    /// Durably store exactly these bytes. Existing objects are verified, never
    /// overwritten. A concurrent writer may cause a transient verification error;
    /// callers may retry but must never treat an error as a durable store receipt.
    pub fn store(&self, bytes: &[u8]) -> io::Result<VaultRef> {
        validate_dir(&self.directory.metadata()?, true)?;
        let reference = VaultRef(sha256::Hash::hash(bytes).to_byte_array());
        match open_relative(
            &self.directory,
            &reference.name(),
            1 | O_CREAT | O_EXCL | O_NONBLOCK,
            0o600,
        ) {
            Ok(mut file) => {
                validate_file(&file.metadata()?)?;
                file.write_all(bytes)?;
                file.sync_all()?;
                validate_file(&file.metadata()?)?;
                self.directory.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let stored = self.load(&reference)?;
                if stored != bytes {
                    return Err(invalid("vault content mismatch"));
                }
                // A duplicate may observe another writer before its sync, so sync
                // the verified file ourselves before issuing a durable receipt.
                let file = open_relative(&self.directory, &reference.name(), O_NONBLOCK, 0)?;
                validate_file(&file.metadata()?)?;
                file.sync_all()?;
                self.directory.sync_all()?;
            }
            Err(error) => return Err(error),
        }
        Ok(reference)
    }

    /// Read and authenticate all bytes. Never return unchecked partial content.
    pub fn load(&self, reference: &VaultRef) -> io::Result<Vec<u8>> {
        validate_dir(&self.directory.metadata()?, true)?;
        let mut file = open_relative(&self.directory, &reference.name(), O_NONBLOCK, 0)?;
        validate_file(&file.metadata()?)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        validate_file(&file.metadata()?)?;
        if sha256::Hash::hash(&bytes).to_byte_array() != reference.0 {
            return Err(invalid("vault content digest mismatch"));
        }
        Ok(bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            // A group-writable developer home is deliberately rejected by the
            // vault. Use the root-owned sticky shared-memory filesystem for the
            // filesystem security tests, never weaken the ancestor policy.
            let base = PathBuf::from("/dev/shm");
            let p = base.join(format!(
                "vault-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&p).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(p)
        }
        fn vault(&self) -> PathBuf {
            self.0.join("vault")
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn only_file(p: &Path) -> PathBuf {
        std::fs::read_dir(p)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }
    #[test]
    fn exact_bytes_survive_restart_and_duplicate_store_is_immutable() {
        let s = Sandbox::new();
        let bytes = b"\0cashu secret\n\xff";
        let v = ProtectedTokenVault::open(s.vault()).unwrap();
        let r = v.store(bytes).unwrap();
        let file = only_file(&s.vault());
        let ino = std::fs::metadata(&file).unwrap().ino();
        assert!(v.store(bytes).unwrap() == r);
        assert_eq!(std::fs::metadata(&file).unwrap().ino(), ino);
        assert_eq!(std::fs::metadata(s.vault()).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(std::fs::metadata(file).unwrap().mode() & 0o7777, 0o600);
        drop(v);
        let v = ProtectedTokenVault::open(s.vault()).unwrap();
        let restored = VaultRef::from_digest(r.digest());
        assert!(v.load(&restored).unwrap() == bytes);
    }
    #[test]
    fn restart_rejects_missing_corrupt_and_permissive_files() {
        for fault in 0..3 {
            let s = Sandbox::new();
            let v = ProtectedTokenVault::open(s.vault()).unwrap();
            let r = v.store(b"private bytes").unwrap();
            let f = only_file(&s.vault());
            drop(v);
            match fault {
                0 => std::fs::remove_file(f).unwrap(),
                1 => std::fs::write(f, b"different bytes").unwrap(),
                _ => std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o644)).unwrap(),
            }
            let v = ProtectedTokenVault::open(s.vault()).unwrap();
            assert!(v.load(&r).is_err());
            if fault != 0 {
                assert!(v.store(b"private bytes").is_err());
            }
        }
    }
    #[test]
    fn rejects_symlink_file_and_hardlink() {
        for hard in [false, true] {
            let s = Sandbox::new();
            let v = ProtectedTokenVault::open(s.vault()).unwrap();
            let r = v.store(b"token").unwrap();
            let f = only_file(&s.vault());
            let other = s.0.join("other");
            std::fs::rename(&f, &other).unwrap();
            if hard {
                std::fs::hard_link(&other, &f).unwrap();
            } else {
                symlink(&other, &f).unwrap();
            }
            drop(v);
            let v = ProtectedTokenVault::open(s.vault()).unwrap();
            assert!(v.load(&r).is_err());
            assert!(v.store(b"token").is_err());
        }
    }
    #[test]
    fn rejects_unsafe_directory_paths_and_modes() {
        let s = Sandbox::new();
        let real = s.0.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = s.0.join("link");
        symlink(&real, &link).unwrap();
        assert!(ProtectedTokenVault::open(&link).is_err());
        assert!(ProtectedTokenVault::open(link.join("child")).is_err());
        assert!(ProtectedTokenVault::open(real.join("../vault")).is_err());
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ProtectedTokenVault::open(&real).is_err());
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ProtectedTokenVault::open(real.join("child")).is_err());
    }
    #[test]
    fn open_descriptor_does_not_follow_replaced_directory() {
        let s = Sandbox::new();
        let v = ProtectedTokenVault::open(s.vault()).unwrap();
        let r = v.store(b"original").unwrap();
        let moved = s.0.join("moved");
        std::fs::rename(s.vault(), &moved).unwrap();
        let other = s.0.join("other");
        std::fs::create_dir(&other).unwrap();
        symlink(&other, s.vault()).unwrap();
        assert!(v.load(&r).unwrap() == b"original");
        v.store(b"new").unwrap();
        assert_eq!(std::fs::read_dir(&other).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 2);
    }
    #[test]
    fn permissions_changed_after_open_fail_closed() {
        let s = Sandbox::new();
        let v = ProtectedTokenVault::open(s.vault()).unwrap();
        let r = v.store(b"token").unwrap();
        std::fs::set_permissions(s.vault(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(v.load(&r).is_err());
        assert!(v.store(b"second").is_err());
    }
    #[test]
    fn accepts_empty_bytes_and_distinguishes_different_content() {
        let s = Sandbox::new();
        let v = ProtectedTokenVault::open(s.vault()).unwrap();
        let a = v.store(b"").unwrap();
        let b = v.store(b"x").unwrap();
        assert!(a != b);
        assert!(v.load(&a).unwrap().is_empty());
    }
}
