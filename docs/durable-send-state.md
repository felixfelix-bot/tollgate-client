# Durable send-once (dormant experimental seam)

`BudgetStore::send_once` is available only with `persistence`. No CLI, JNI,
watcher, wallet or production transport calls it. It does not enable automatic
spending and does not make the legacy manual payment paths safe.

## Transaction and ownership

One BEGIN IMMEDIATE transaction reserves face plus additional fee bound, inserts
the stable operation/network/vault identity, claims every `(canonical mint,Y)`,
and persists `post_may_have_started`. Only successful commit of that new row
invokes the injected FnOnce transport. Reservation success alone is never send
permission; existing accounting-only reservations cannot be adopted. Exact
repeats (including different caller attempt IDs and reordered proofs) return
status. Changed operation payloads conflict; overlapping proofs fail atomically.
Claims are permanent; no lease expires and no ownership takeover exists.
Consequently no network-spanning mutex is needed in this restricted slice: even
while the first sender is alive, competitors cannot acquire its right to send.
Disjoint operations may execute concurrently, within transactionally held caps.

Every send stays quarantined with its full exposure, even after grant notices,
refusals, or all-spent/all-unspent observations. Existing settle/release methods
reject send-owned reservations. There is deliberately no recovery release path.
A crash after the mark but before bytes is indistinguishable from delivery;
reopen only reports status. Failure saving a response leaves the prior Unknown.
All-spent is sticky but proves neither which party consumed value nor access.
`internet_verified` is always false. No automatic funding or refusal guessing.

## Explicit trusted boundary — NOT production-safe validation

`TrustedSendPlan` and `TrustedProofIdentity` are caller assertions. Basic bounded
labels, unique Y, checked amounts and face-sum checks are NOT Cashu validation.
The caller must establish canonical mint identity (aliases must not bypass
ownership), actual hash-to-curve Y, valid keyset/unit/proofs, stable entitlement
and network/generation identity, durable protected vault object and exact token
binding. The transport must post that exact token once on the bound network;
this module cannot constrain a malicious callback. Attempt ID is intentionally
ignored. No secrets or token bodies are persisted in this ledger.

`observe_proofs` is only a trusted aggregate diagnostic snapshot, NOT a NUT-07
adapter: it does not validate complete per-proof observations or retain their
history. It cannot authorize send/settle/release. Real parsing, secure vault,
per-proof reconciliation, settlement, network binding and access verification
are later integration gates. Never connect untrusted pasted strings to this API.

## Schema and operational limits

Schema v2 adds only sends/proof_claims; existing accounting tables retain their
shape. Exact schema, foreign keys, proof face totals/mint agreement and absence
of terminal ledger entries for sends are checked on open. v1 and unknown versions
fail closed, without mutation/reset or automatic migration. Preserve existing
v1 files and reconcile separately; do NOT recreate them as empty v2 ledgers.
Older v1 binaries reject v2. No production migration is supplied.
Private local durable storage, honest fsync, trusted days/caps, no hostile SQL
writer and no ledger rollback/deletion remain prerequisites. Ownership covers
one installation, not other holders of copied bearer proofs.

## Measured evidence

Initial six-test RED compiled against the base: exit 101, missing send_state and
send_once (compile RED, not a behavioral failure). GREEN expanded to eight tests:
attempt/reorder replay, overlap/subset rejection, accounting replay exclusion,
panic at injected transport entry then reopen, two actual OS-process callers,
SQL-abort before mark => zero callbacks and zero exposure, response-write failure
=> one callback and retained ambiguity, spent/access distinction, old version.
Failure injection uses SQLite abort triggers, not a real ENOSPC/fsync fault.
The send interruption test unwinds at transport entry, not SIGKILL; inherited
seven budget crash tests still exercise real SIGKILL and remain green.

Full persistence and all-features suites: 84 top-level tests each, excluding four
child worker executions. Minimal suite: 61. Android host tests: 2, zero buys.
Direct LLVM instrumentation of send_state suite: send_state.rs 85.71% lines,
79.36% regions, 87.50% functions. Branch coverage unavailable (0 recorded).
No device, power-loss, mint, real transport or internet evidence; no funds used.
