## Ledger storage hardening

- Added stable trusted-ancestry validation and explicit SQLite Unix VFS NOFOLLOW opens; unsafe ledger/auxiliary nodes and stranded enrollment recovery fail closed.
- RED: five regression tests failed before fix. GREEN: seven storage cases; full harness, production persistence and minimal suites pass, including concurrency and SIGKILL recovery.
- Instrumented storage coverage: 92.86% lines, 91.18% regions, 100% functions. Evidence logs remain local in the ledger-storage worktree.
- See docs/ledger-storage.md for exact permissions, VFS source semantics, test commands and exclusions. Same-UID/root, rollback, power loss and device acceptance remain excluded; no funds/transport/autobuy changes.

## Policy/ledger binding hardening

- Strict duplicate rejection now includes nested cap-map keys (top-level duplicate and unknown fields already use serde strict structs). Caps/fees are integer u64 values bounded by SQLite i64, never floating-point budgets.
- Schema v4 stores canonical enrolled policy, random installation identity and canonical ledger location together in one SQLite transaction. A strict private sidecar carries the matching identity/policy; it is synced before commit. Missing/partial enrollment, swapping either component, copied unrelated pairs, and editing both expected/sidecar policy fail closed. No repair, reset, relocation or migration command is provided; v3 state is rejected, not discarded.
- RED actual CLI tests reproduced duplicate sat-key acceptance and swapped-ledger acceptance; GREEN full all-features suite includes eight CLI subprocess tests (including restart, partial enrollment and SIGKILL send boundary). No-default-features suite also passed.
- Direct LLVM coverage using the five-test subprocess subset: offline_policy 195/205 lines (95.12%), 311/364 regions (85.44%); not full-suite or branch coverage. cargo-llvm-cov is unavailable; matching rustup LLVM tools used.
- Recording enrollment requires a new simulation directory, rejects --dry-run/legacy options, and never adopts an existing wallet directory. The standalone simulator remains explicitly offline. No real transport, money, deployment or auto-buy enablement occurred.
- Remaining gate: ledger symlink/TOCTOU hardening (including SQLite sidecars) is NOT solved. Trusted local storage/clock and no hostile same-UID mutation or rollback remain assumptions; this binding is not tamper-proof. After that gate, integrate the pre-funded wallet -> durable reservation -> exact-byte bound-interface send -> session observation/reconciliation vertical path, not another disconnected simulation.
- Evidence: ~/reports/reviews/policy-binding-{red,green,full,minimal}.log. Static independent review found no blocker within the declared exclusions.

## Actual CLI recording-only continuation

- Added strict opt-in `tollgate-agent --recording-only`, dedicated new-directory enrollment,
  typed simulated JSON and durable recording callback; existing offline adapter reused.
- RED: four actual-binary tests failed before implementation (missing --gate / legacy auto success).
  GREEN: 3 normal subprocess tests, 4 with test-only SIGKILL checkpoint; separate feature-absence test passes.
- Full locked/offline persistence, nonproduction-test-harness and no-default-features suites exit 0.
  Default feature-absence test also passes. Existing unrelated warnings remain.
- LLVM direct instrumentation of CLI: recording module 156/162 lines (96.30%), 217/250 regions
  (86.80%), 10/11 functions (90.91%); three normal subprocess tests, no branch coverage.
  cargo-llvm-cov unavailable; used installed matching LLVM tools instead.
- No HTTP/payment/funding/device actions, watcher changes or new PR. Trusted clock and local-storage
  exclusions remain; independent review and real monetary integration are later gates.
- Usage and threat boundaries: docs/cli-recording.md. Logs: cli-recording-{full,harness,minimal}.log
  in manager scratch. Process tests use tmpfs, not power-loss durability evidence.

# Offline policy adapter — simulation only

- Added immutable enrolled per-unit caps, fee bound, snapshot digest and freshness interval.
- CLI uses exact-byte validated send with a fake digest-only callback; no network payment path.
- Six subprocess tests passed: restart exposure, full face plus fee, changed policy, aliases/overlap, vault corruption, snapshot freshness/hash and unknown unit.
- RED: executable absent caused six failures; raw-API compile-negative fence failed when method was public. GREEN: cargo test --features persistence and cargo test --features nonproduction-test-harness passed.
- Raw API requires explicit nonproduction-test-harness feature (or unit tests); production doctest proves it unavailable. Existing legacy tests retained behind that feature.
- No production enablement: enrollment does not authenticate metadata; remote provenance, authenticated clock, spendability, interface binding and verified access remain gates.

# Progress

## Offline verified-byte boundary (2026-10-30)

- `send_validated` re-loads digest-checked immutable vault bytes, rederives bearer
  claims against the snapshot, rejects mutable-plan mismatch, durably reserves
  full face plus trusted fee and claims ownership before typed transport sees
  exactly those bytes. Legacy raw API renamed `send_once_nonproduction`.
- RED reproduced all five mutable-claim/payload bypasses on the prior raw seam;
  GREEN: 109 all-feature tests, 33 library; no-CDK persistence suite also passes.
  Typed tests cover corrupt/missing/different vault objects, undercount, aliases,
  retry/restart and panic quarantine. LLVM: new boundary 100% lines/functions,
  98.39% regions; no branch coverage claim. Cold review: no bounded-scope blockers.
- Snapshot/fee authenticity, network binding, policy and persistence/platform
  assurance remain deferred. No CLI/JNI activation, real funds or deployment.
  See [contract and evidence](docs/verified-send.md); earlier milestones below
  describe their historical state, not permission to spend.

## Canonical mint identity repair (2026-10-30)

- Shared real URL-parser policy fixes default-port alias ownership splits in
  snapshot/token matching, overlap, typed plans and send-once persistence.
- Schema 3 refuses all legacy schemas without migration; canonical stored mint
  identities are audited on open. Existing exposure must not be discarded.
- RED: exact two-unmodified-plans probe on 63a3982 invoked transport twice,
  failing expected-one assertion. GREEN: `cargo test --locked --all-features`
  passed 106 top-level tests (30 library); existing history_wiring unused imports
  warning remains. See [identity contract](docs/mint-identity.md).
- Mutable-plan/payload substitution and proof authenticity remain production
  blockers/limits. No CLI/JNI enablement, funds, deployments or integration.


## Offline validated-token and protected-vault milestone (base 920a462)

- Added `validated_token` (CDK feature) and Linux/Android `token_vault`; no CLI/JNI/watcher wiring.
- Cashu/CDK 0.18.3 locked types implement NUT-00 decoding, NUT-02 keyset consistency and canonical NUT-07 Y via CDK hash-to-curve. Trusted snapshots bind mint, unit, keyset and denomination keys; sums are checked, duplicate Y rejected and overlap compares mint + Y.
- Typed adapter derives face value from decoded proofs, rejects unknown fee bounds before storing, and persists exact bytes before returning the dormant legacy plan.
- Vault uses pinned directory descriptors, no-follow traversal, exclusive 0600 files under 0700 directory, SHA-256 verification and file/directory fsync. Existing objects never overwritten; unsafe ancestry, symlinks, hardlinks, missing/corrupt objects fail closed.
- Worker TDD evidence: parser 7 failing tests then green; adapter 1 failing then green; vault 6 failed/1 passed then 7 green. Fixtures are synthetic proofs constructed with CDK hash-to-curve/signing, no real funds.
- Independently rerun `cargo test --locked --all-features --quiet`: 103 top-level tests passed; `cargo test --locked --no-default-features --quiet`: 61 passed. New tests total 15. Read-only review found no blocking issue.
- Coverage not measured: `cargo llvm-cov --version` failed because the subcommand is not installed. No coverage percentage claimed.

Limits: structural validation is NOT issuer/DLEQ authentication, spend-state knowledge, spending-condition satisfaction, metadata freshness or network/entitlement authentication. Fee bound is explicitly trusted caller input. The old public mutable TrustedSendPlan remains bypassable and unfit for production callers. Vault is not encrypted or resistant to same-UID/root tampering or rollback; interrupted writes remain fail-closed residue, not auto-repaired. Host-only filesystem tests, no power-loss/Android persistence proof. Group-writable ancestors (including this host home) are deliberately rejected; tests use root-owned sticky /dev/shm. No mint/router calls, production funds, new PR or deployment.

## Send/terminal TOCTOU correction

- Ownership check now runs under the same BEGIN IMMEDIATE as release/settle replay and insertion.
- Four behavioral regressions observed failing on 03bbbfed: two deterministic lock-boundary tests and two 2,000-send public API races.
- Fixed: zero bypasses, all exposure retained across day rollover/reopen, replay never calls transport.
- Persistence/all-features: 88 top-level tests each; default/minimal: 61 each; targeted crash: 7; Android host: 2. All passed.
- Watchers remain discovery-only; no new feature, funds, deployment or PR.

## Durable send-once safety slice

- Atomic reserve, permanent proof ownership and post_may_have_started before callback.
- Replay/status only; all-unspent and refusals hold exposure, never authorize resend.
- Trusted proof/vault/network input seam is NOT production-safe validation.
- Schema v2 rejects v1/unknown without migration/reset; CLI/JNI/watch unchanged.
- Eight new tests; persistence/all-feature suites 84 each; minimal 61; Android host 2.
- See docs/durable-send-state.md for measured coverage and integration gates.

## Linux process-crash follow-up

- Six parent scenarios plus subprocess entrypoint; deterministic pipe handshakes,
  20-second checkpoint deadlines and verified SIGKILL exits. No sleeps.
- Actual pre-commit/post-commit reserve and settle, repeated replay, spilled
  uncommitted SQL rollback, stale-open competitor, interrupted real initialization.
- Persistence/all-feature suites: 76 top-level passed each; minimal: 61.
- No production changes. Process crashes only, not power-loss/Android proof.
- Branch pr/budget-crash-tests based on 5150c24; no second PR created.

## Durable budget-store foundation

- Added opt-in persistence feature with versioned SQLite accounting, checked
  integer face-value plus fees, all-day exposure, idempotent settlement/release.
- Eight store tests include real competing child processes, reopen, overflow,
  rollover exposure, unreadable permissions and structural/logical corruption.
- Full all-features and persistence-only suites: 69 passed each; no-default: 61.
- No CLI/JNI/wallet changes; watcher remains discovery-only. See docs/budget-store.md.
- Remote publication handed to manager under the worker remote-mutation policy.

## Android foreground safety

- Preserved original dirty checkout and copied its two-file patch into an isolated
  `pr/android-safe-foreground` worktree.
- Kept Android 14 dataSync permission; replaced unsafe background fallback with
  stopSelf + START_NOT_STICKY and no worker after foreground failure.
- Removed automatic payment from the watcher, including the minute-based attempt
  ID generator. Updated UI messages to state discovery-only behavior.
- Added executable JVM lifecycle/no-spend regression and manifest permission test.
- Passed 2 Android host tests, Android-34 javac compile, and 61 Rust tests in each
  default and Android feature configurations.
- Remaining: exact-build device test, persistent spend ledger, token identity and
  reconciliation, structured JNI JSON, safe funding UX. No production-readiness claim.
