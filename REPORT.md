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

# Safety milestone report

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

## Send/terminal race correction (base 03bbbfed)

Isolated branch `pr/send-terminal-race`; dirty main and reviewed source untouched.
`finish` now acquires BEGIN IMMEDIATE before checking send ownership, including
before terminal replay. Both release and settle share this path. Audit: reserve
and send_once already validate replay/ownership under immediate transactions;
observe_proofs is a single sticky UPDATE and delivery update does not change
accounting/ownership. No other terminal insert path exists in production.

Observed behavioral RED with original production logic (test module only added):
- Deterministic release AND settle: terminal Ok, callback 1, usage 0, reopen false.
- Public API stress, 2,000 sends each: release bypasses 1,994 / usage 6;
  settle bypasses 1,902 / usage 98; both failed reopen. Both commands exited 101.
- Deterministic test holds a real RESERVED lock, waits for the terminal connection's
  SQLite busy handler via channels, commits a public send before resuming BEGIN.
  No sleeps, fabricated ledger rows, production hooks or mocked transactions.

GREEN: deterministic tests return Conflict, one callback, usage 9 and successful
reopen/status-only replay. Both stress tests: zero bypasses, 2,000 callbacks,
usage 2,000 on the following day, successful reopen with unchanged usage.
Commands executed directly with exit status preserved, no tee/true masking:
- `cargo test --locked --no-default-features --features persistence -- --nocapture`: 88 top-level passed.
- `cargo test --locked --all-features`: 88 top-level passed.
- `cargo test --locked --no-default-features` and `cargo test --locked`: 61 each.
- `cargo test --locked --no-default-features --features persistence --test budget_crash -- --nocapture`: 7 passed.
- `python3 tests/android_service_test.py`: 2 passed, zero buys.
- `cargo fmt --check`, `git diff --check`: exit 0. Existing unrelated warnings remain.
Counts exclude child-worker invocations. Logs in manager cache/scratch:
`terminal-red-{boundary,stress}.log`, `terminal-green-{persistence,all,minimal,default,crash,android}.log`.
No new coverage measurement, power-loss, Android persistence, funds or production
transport claims. Watchers remain discovery-only; no new PR, merge or deployment.


## Durable send-once evidence

Based on b0d5eef, isolated pr/durable-send-state worktree. Initial test command
exited 101 (missing module/API compile RED); after implementation eight new tests
pass, including two OS-process owners producing exactly one fake POST.
Full `cargo test --locked --no-default-features --features persistence` and
`cargo test --locked --all-features`: 84 top-level tests each (four child executions
excluded). Minimal no-default: 61. Android JVM tests: 2, zero buys.
`cargo fmt`, `git diff --check`: passed. Existing unrelated warnings remain.
Direct LLVM profile: send_state.rs lines 85.71%, regions 79.36%, functions 87.50%;
no branch coverage. Instrumented send suite only, not all-suite coverage.
Logs: manager scratch send-{red,persistence,all,minimal,cov}.log; send.profdata.
No production parsing/vault/transport/reconciliation or spending enablement.
Pre-mark SQL-abort produces zero callbacks; response-write failure retains
ambiguity; interruption test is panic/reopen, not process-kill at the send mark.
No second PR, funds, devices or deployment. See docs/durable-send-state.md.

## Linux process-crash follow-up

Read the existing tests, cold review and payment safety design first. Added six
parent scenarios plus one subprocess entrypoint. Existing production behavior is
being regression-tested; no production change or fabricated RED evidence claimed.

- Pipe handshakes have 20-second receive deadlines; child cleanup kills/reaps on
  failure. Explicit deaths assert Unix signal 9, not graceful drop/panic exits.
- Real reserve/settle tested before invocation, at rollback-journal sync before
  commit completes, and after successful API return. Reopen/replay repeats three
  times; settlement stays at 7, not 14; prior reservation remains 9 if interrupted.
- Raw SQLite transaction changes reservation, operations and watermark, forces
  dirty-page spill with real sqlite3_db_cacheflush, then dies without rollback.
  Reopen restores committed exposure and allows the original day.
- Competing process opens before another process reserves, then tries admission
  after holder death: 2 against outstanding 9/cap 10 must return CapExceeded.
- Actual create is interrupted after initial file sync and schema-journal sync;
  repeated open and create both fail closed without deleting/re-enrolling.

The Linux LD_PRELOAD observer in tests/support/crash_sync.c forwards successful
real fsync/fdatasync first, checks the exact /proc/self/fd pathname, signals the
parent, then blocks for SIGKILL. It does not fake syscall success, alter writes,
locking or SQLite configuration, or install any production hook. cc builds it
with warnings-as-errors. This samples two initialization boundaries and one
pre-commit boundary, not every syscall. Fully committed creation should reopen.
The raw-SQL test's small cache is only to exercise spill; it is complementary
SQLite rollback evidence, not a production cache-size claim.

Actual executions, exit 0 each:
- cargo test --locked --no-default-features --features persistence: 76 top-level passed.
- cargo test --locked --all-features: 76 top-level passed.
- cargo test --locked --no-default-features: 61 passed (crash suite disabled).
- Targeted budget_crash: 7 passed (six scenarios and child entrypoint).
Counts exclude existing budget_store subprocess test invocations. Full suites
run under timeout 240. Logs: manager scratch budget-crash-{persistence,all,minimal}.log.
Existing unused imports/dead-code warnings remain; no coverage gain inferred.

Scope is Linux process-kill/reopen, NOT power-loss, kernel-cache loss, hardware
fsync honesty, Android/NDK, network filesystem or payment readiness evidence.
Linux tests require cc, dynamic loading and /proc; other platforms compile them
out. No payment/device/router changes; discovery-only remains. No second PR.


## Durable budget foundation evidence

- Initial RED: `cargo test --no-default-features --features persistence --test budget_store`
  exited 101: missing budget_store module (compile RED, not behavioral evidence).
- Independent read-only review identified incomplete-schema/logical-corruption
  acceptance. Regression first failed `accepted DROP INDEX one_terminal` (101);
  exact-schema and accounting validation then made it GREEN.
- Final `cargo test --all-features`: 69 passed; persistence-only full suite:
  `cargo test --no-default-features --features persistence`: 69 passed.
- `cargo test --no-default-features`: 61 passed. Android host tests: 2 passed,
  repeated ticks/restarts zero buys. rustfmt checks and git diff --check passed.
- Coverage unavailable: `cargo llvm-cov --version` exited 101, command not installed.
  No coverage percentage claimed. Existing unused-import/dead-code warnings remain.
- Added src/budget_store.rs, tests/budget_store.rs, docs/budget-store.md, explicit
  optional rusqlite feature and module export. No CLI/JNI payment wiring.
- Limits: caller-authorized caps/units/days, private local filesystem, no malicious
  external SQL writers or DB rollback. No proof/engine, forward-clock validation,
  migration, partial settlement or NDK persistence acceptance. Watcher stays off.
- Branch pr/durable-budget-store is based on d801a3f; manager must perform remote
  mutations under repository workflow. No remote publication claimed by worker.

# Prior Android safety report

## Outcome

Foreground permission fixed; startup fails closed. Android background watch is
strictly discovery-only, with no automatic-spending opt-in. Saved tokens remain
untouched. No real funds, router changes, or key handling were performed.

## Evidence

- RED: original dirty service failed JVM regression at `refused foreground must stopSelf`.
- GREEN: `python3 tests/android_service_test.py` -> `Ran 2 tests ... OK`;
  lifecycle harness -> `PASS: permission/platform refusal, single watcher, destroy,
  repeated ticks/restart: zero buys`.
- `javac -classpath ~/android-sdk/platforms/android-34/android.jar` over all Android
  Java sources -> exit 0.
- `cargo test` -> 61 passed, 0 failed (subprocess exit 0).
- `cargo test --no-default-features --features android` -> 61 passed, 0 failed.
- Existing Rust warnings (unused imports/dead code) remain unrelated.

## Risks and boundaries

Former minute-based attempt IDs allowed repeated ambiguous-token sends. FileWallet
cannot reconcile with NUT-07; JNI resets spent_today and hand-formats JSON. These
are not repaired by this bounded patch: the watcher payment call is removed.
Host JVM doubles are not Android instrumentation; this exact build still needs an
emulator/device run. No APK was signed because key handling was excluded. Prior
mock grants are not proof of payment settlement, durable budgets or access.
See docs/android-verification.md for release limitations. Not production ready.

## Publication

Prepared on `pr/android-safe-foreground`. Manager must review, push and open a PR
(manager-only remote-mutation policy); no remote success is claimed here.
