# Bounded ledger filesystem hardening

BudgetStore create/open now refuse unsafe storage before SQLite opens recovery files.
The ledger and existing `-journal`, `-wal`, `-shm` must be single-link regular files,
effective-UID-owned, exactly mode 0600 (including no special bits). Missing auxiliaries
are normal. Enrollment refuses *any* existing database or recovery file, including
corrupt/empty ones; it does not repair, delete, reset, or re-enroll them.

## Stable pathname boundary

Paths are made absolute without canonicalizing symlinks. `..`, symlink ancestors,
foreign-owned ancestors, and group/other-writable ancestors are rejected. Root-owned
sticky ancestors (e.g. `/dev/shm`) are allowed: their next component must itself be
root/euid-owned. The final parent must be euid-owned and not group/other writable;
0700 is recommended (readable/searchable 0755 is accepted; bearer/database files
remain 0600). Existing insecure installations fail closed; no automatic chmod.

This is an inductive namespace-stability argument, not merely leaf lstat followed by
an unprotected open: root is trusted; a checked non-writable parent prevents another
UID replacing its child while that child is checked; the sticky exception prevents
another UID renaming a root/euid-owned child. Thus no accepted ancestor or final name
can be replaced by the declared adversary between validation and SQLite open, or
while SQLite later creates/reopens auxiliaries. Trusted processes must not chmod,
rename, delete or mutate these paths during use. No hostile same-UID/root resistance.
Filesystems must implement local Unix ownership, permissions, sticky rename and
SQLite locking correctly. Network/user-controlled mounts are outside this boundary.

## Actual SQLite mechanism (not fd/VFS invention)

Connection uses `open_with_flags_and_vfs(..., READ_WRITE | NO_MUTEX | NOFOLLOW,
"unix")`, without CREATE or URI. This pins the bundled Unix VFS rather than relying
on a configurable default. The locked rusqlite 0.31/libsqlite3-sys 0.28 amalgamation:
- `unixFullPathname` resolves components and reports `SQLITE_OK_SYMLINK`;
- pager open translates that to `SQLITE_CANTOPEN_SYMLINK` with OPEN_NOFOLLOW;
- `unixOpen` adds OS `O_NOFOLLOW` to its open flags.

A direct rusqlite regression proves main-file and ancestor symlinks fail with these
actual flags/VFS. NOFOLLOW alone does **not** guarantee ancestor race safety: the
ownership/mode chain is essential. No `/proc/self/fd` alias is passed to SQLite;
full-path resolution would undermine assumptions about descriptor-relative names.
A custom dirfd VFS or privileged broker would be needed for a stronger mutable-tree
boundary, with separate lock, auxiliary, recovery and durability qualification.
Unsupported platforms fail closed. Android is compile-target intent, not on-device
filesystem/power-loss acceptance.

SQLite DELETE journal mode, synchronous FULL, accounting schema, enrolled policy and
installation-path binding, and simulation namespace are unchanged. No ATTACH API is
exposed, so arbitrary super-journal namespaces are not introduced. We check the fixed
main journal/WAL/SHM paths, not arbitrary temporary files. This does not authenticate
file contents against same-UID mutation, disk rollback, root, or prove power-loss
immunity. Watchers remain discovery-only.

## Evidence

`ledger-storage-red.log`: five tests added before implementation, all failed.
`ledger-storage-full.log`, `ledger-storage-production.log`, and
`ledger-storage-minimal.log`: full harness, production persistence, and minimal
persistence suites pass, including SIGKILL/reopen and concurrent budget/send tests.
Seven filesystem tests cover symlinks, hardlinks, nonregular nodes, unsafe modes,
ancestor replacement churn in an already-unsafe tree (always refused), SQLite's
NOFOLLOW semantics, and stranded recovery refusal. Churn is driven by the test UID
in an adversary-writable directory; it is **not** a distinct-UID exploit test nor a
claim to withstand same-UID replacement in an accepted tree. Ownership predicates
are implemented; foreign-owner mutation is not exercised by this unprivileged suite.

Run with `umask 077; TMPDIR=/dev/shm cargo test --locked --features
nonproduction-test-harness` (and `--features persistence`, or
`--no-default-features --features persistence`). This host's home is 0775 and default
umask creates 0775 directories, intentionally rejected. No host permissions changed.
/dev/shm crash tests are process-crash evidence, not durable-media evidence.
Coverage uses rustc instrument-coverage plus installed llvm-profdata/llvm-cov; the
seven-test filesystem run covers ledger_storage.rs 92.86% lines, 91.18% regions,
100% functions (`ledger-storage-coverage-summary.log`). cargo-llvm-cov was absent;
raw toolchain instrumentation was used instead.
