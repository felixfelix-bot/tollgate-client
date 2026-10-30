# Progress

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
