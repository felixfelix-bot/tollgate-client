//! Pathname safety for SQLite's bundled Unix VFS, not an fd-backed VFS.
//! Trusted root/euid-owned ancestry makes pathname resolution stable against
//! other UIDs. Root and hostile same-UID writers (including chmod) are excluded.
use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn checked_path(input: &Path, creating: bool) -> io::Result<PathBuf> {
    use std::{fs, os::unix::fs::MetadataExt, path::Component};
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: no arguments or preconditions.
    let uid = unsafe { geteuid() };
    let bad = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe ledger storage path",
        )
    };
    if input.as_os_str().is_empty() {
        return Err(bad());
    }
    let absolute = if input.is_absolute() {
        input.to_owned()
    } else {
        std::env::current_dir()?.join(input)
    };
    // Do not canonicalize: that would hide symlinks and parent traversal.
    let mut names = Vec::new();
    for part in absolute.components() {
        match part {
            Component::RootDir | Component::CurDir => (),
            Component::Normal(name) => names.push(name),
            _ => return Err(bad()),
        }
    }
    if names.is_empty() {
        return Err(bad());
    }
    let mut path = PathBuf::from("/");
    // Validate each ancestor before resolving its child. In a root-owned sticky
    // directory, a child owned by root/euid cannot be renamed by another UID.
    // Else no ancestor may be group/other writable. This is an inductive
    // stability condition, not a check-then-open race defense against same UID.
    for (index, name) in names.iter().enumerate() {
        let meta = fs::symlink_metadata(&path)?;
        let mode = meta.mode() & 0o7777;
        let sticky_root = meta.uid() == 0 && mode & 0o1000 != 0;
        let final_parent = index + 1 == names.len();
        if !meta.is_dir()
            || (meta.uid() != 0 && meta.uid() != uid)
            || (mode & 0o022 != 0 && !sticky_root)
            || (final_parent && (meta.uid() != uid || mode & 0o022 != 0))
        {
            return Err(bad());
        }
        path.push(name);
    }
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        match fs::symlink_metadata(Path::new(&name)) {
            Ok(meta) => {
                if !meta.is_file()
                    || meta.uid() != uid
                    || meta.mode() & 0o7777 != 0o600
                    || meta.nlink() != 1
                {
                    return Err(bad());
                }
                // Enrollment must not adopt leftover SQLite recovery files.
                if creating {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "ledger or recovery file exists",
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && (creating || !suffix.is_empty()) => (),
            Err(e) => return Err(e),
        }
    }
    Ok(path)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub(crate) fn checked_path(_: &Path, _: bool) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "ledger filesystem safety requires audited Linux/Android Unix VFS",
    ))
}
