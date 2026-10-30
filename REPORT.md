# Safety milestone report

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
