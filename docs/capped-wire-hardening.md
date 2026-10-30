# Capped purchase wire and evidence bounds

Evidence records are fsynced before publication by an atomic hard link, which
cannot replace an existing destination. Temporary and destination collisions
are retried, including reopened sinks and concurrent publishers. Temporary
files are removed and the directory is fsynced before success is returned.
Files remain 0600 in a required 0700 directory. This is append-only publication,
not protection against an owner subsequently editing/deleting their files.

Session reads cap total connect/write/read/EOF verification time at 10 seconds
(or the caller's smaller total). Connect uses the smaller of its own limit and
the remaining total; every partial write resets its socket timeout to the
smaller of the write limit and remaining total. Zero budgets fail closed.
Polling normalizes both constructors and public struct literals to 1–16
attempts and 0–1 second intervals: at most 175 seconds of network waits and
inter-attempt sleeps (plus scheduling/local processing overhead).

Only HTTP/1.0 and HTTP/1.1 status lines with space delimiters are accepted.
Control characters in header values are rejected (horizontal tab is permitted).
Content-Length remains mandatory; transfer encoding remains unsupported.
After exactly the declared body, the reader requires EOF within the same total
deadline because it requested `Connection: close`. Extra bytes, including a
later TCP packet, fail closed. A peer that leaves the connection open instead
of closing now times out; this intentionally stricter compatibility rule
prevents packet-boundary-dependent acceptance. Body allocation stays bounded.

Both session and notice JSON reject duplicate object members recursively,
including escaped spellings of the same key. Unknown unique fields remain
accepted. These changes do not establish device attribution, replay provenance,
metric semantics, or actual data-plane connectivity; those are separate gates.

## Regression evidence

Before implementation, `cargo test --features persistence --lib wire_tests`
compiled and failed all six initial tests: reopened evidence replacement,
duplicate JSON, invalid HTTP, excessive poll parameters, expired-total connect,
and trailing bytes delivered after the headers. Additional socket write-stall
and public zero-attempt poll tests exercise the hardened paths. All tests use
loopback and private `/dev/shm` test data; no real payment is made.
