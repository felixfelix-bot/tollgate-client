# Safety milestone report

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
