# Progress

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
