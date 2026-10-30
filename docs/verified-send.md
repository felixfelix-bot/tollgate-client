# Offline verified-byte send boundary

`BudgetStore::send_validated` is a dormant, structurally validated send API,
not payment permission. No CLI/JNI/watcher integration or production transport
is included. Requires CDK, persistence and Linux/Android.

The mutable `TrustedSendPlan` is only a proposal. At each call the API decodes its
SHA-256 reference, loads and digest-checks the exact vault object, reparses it
against the private trusted snapshot, and rederives face, proof Y/keyset/amount,
canonical mint, unit and vault reference. Any mismatch rejects before reservation
or transport. It reserves decoded full face plus the explicitly trusted fee bound,
not advertised cost. A single transaction reserves exposure, claims mint/Y and
persists the irreversible send mark before transport receives those same loaded
bytes. It never accepts a separate caller payload or closure. `VerifiedBytesTransport`
gets only an immutable byte slice. Implementations remain trusted: Rust traits
cannot prevent a malicious implementation from ignoring arguments or doing other
I/O. There is no production implementation or blanket closure adapter.

Exact replay is status-only. Changed intent/reference conflicts, overlapping
proofs, aliases, restart and panic cannot acquire another send. Missing/corrupt
vault objects reject even during replay. Delivery observations do not release
exposure. The old raw harness is explicitly named `send_once_nonproduction`;
the verified API does not call that public harness, but shares its crate-private
transaction engine with fully rederived inputs.

## Evidence

Before implementation the behavioural regression on the original raw seam failed
with `accepted bypasses: [0, 1, 2, 3, 4]`: face undercount, forged proof Y, changed
vault reference, changed mint and substituted transmitted bytes. After switching
the same regression to the typed verified boundary it passes. Additional typed
fake tests cover different existing vault objects, V3 reencoding, corrupt/missing
bytes, malformed references, unit mismatch, overflow, full-face cap rejection,
restart/replay, canonical aliases, new-operation overlap and transport panic.
All fixtures are synthetic; transport only records bytes locally.

`cargo test --locked --all-features`: 109 top-level tests pass (33 library),
excluding child worker repetitions. No-default-features plus persistence also
passes. Existing history_wiring unused imports and no-CDK TestMintPayer warnings
remain. `cargo fmt --check` and `git diff --check` pass.
Direct rustup LLVM instrumentation of all-feature library tests reports
validated_send.rs: 41/41 lines, 3/3 functions, 61/62 regions (98.39%); no branch
coverage recorded. This file has no in-file tests. Delegated cold review found
no blockers within this bounded scope; backing model family was not established.

## Deferred production gates

Snapshot provenance/freshness, issuer signatures/DLEQ, spending conditions,
fee-policy authenticity/derivation, authenticated network/entitlement binding,
and trusted budget policy remain unresolved. Caller fee/day/cap/network inputs
are not authenticated by this API. A structurally valid bearer is not necessarily
spendable. Untrusted import resource bounds, malicious same-UID/root writers,
disk rollback, injected ENOSPC/fsync, power loss and Android execution are not
proven. Tests use tmpfs and do not prove disk durability. Existing legacy payment
paths are not repaired or approved. **Do not use this milestone for real spending.**
