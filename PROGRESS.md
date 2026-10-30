# Progress

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
