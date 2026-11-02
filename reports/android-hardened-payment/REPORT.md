# Android hardened payment — first bounded milestone report

Branch `pr/android-hardened-payment` from live `felixfelix-bot/tollgate-client` main
`e49dcec27f48ed251b398d10d8d05bbb5a67eba9`. Worktree
`/home/c08r4d0r/worktrees/tc-android-hardened-payment`. Nothing merged; no funds,
lab, FIPS or channel changes. Scope froze to milestone 1 (platform feasibility and
legacy JNI bypass closure).

## Verified here

### 1. Legacy JNI buy cannot POST — real JVM, real socket (RED then GREEN)

`tests/android_jni_test.py` loads the actual `cdylib` into a host JVM, starts a
loopback gate that records every request, calls `Core.describe`, then three
`Core.buy` variants (dummy note, quote/backslash/newline payload, all-null
arguments), then `Core.capsFor` with an escaping payload. Assertions come first
from the **server's** observation, then from `json.loads` of each returned
document.

- RED (pre-fix source, `reports/android-hardened-payment/jni-red.log`):
  requests were `['GET','GET','POST','GET','POST']` — two real legacy POSTs with a
  dummy token, i.e. the bypass was live, not hypothetical.
- GREEN (this branch, `jni-green.log`): exactly one discovery `GET`, zero POSTs;
  each buy returns the fixed document
  `{"state":"not-attempted","code":"payment-integration-unavailable"}`; the
  caller-supplied journal path is never created; `capsFor` returns typed JSON with
  the raw unit echoed exactly (escaping preserved).
- GREEN was also observed against the full `android-hardened` feature build, so
  the tombstone holds when cdk/persistence are compiled in, not only in the
  discovery build.

### 2. Explicit hardened feature and build contract

- `Cargo.toml`: `android = ["dep:jni","dep:serde","dep:serde_json"]`;
  `android-hardened = ["android","cdk","persistence"]`. The test harness feature is
  never implied.
- `packaging/android/build-native.sh` (new): `--lib --locked`, NDK
  `26.1.10909125` (`ANDROID_NDK_HOME` overridable), API 26, both
  `aarch64-linux-android` and `x86_64-linux-android`, with per-target
  `CC_*`/`AR_*` (bundled SQLite + ring) and `CARGO_TARGET_*_LINKER`. It refuses
  any feature other than `android|android-hardened`.
- `packaging/android/build-apk.sh` now calls that helper and copies `.so` files
  from the derived `CARGO_TARGET_DIR` instead of a hardcoded `target/`. Checked
  by `tests/android_build_test.py` (RED first: the script did not exist).

### 3. Existing shell regressions unchanged

`tests/android_service_test.py`: manifest `dataSync` permission/type, foreground
refusal, single watcher, destroy, repeated ticks/restart with a stored dummy note
— zero `Core.buy` calls. Passing.

### 4. Host suites

`cargo test --locked --features android-hardened` was run. Result recorded in
`host-full-tests.log`; see "Build/CI caveats" for what this run can and cannot
claim.

## Not verified — do not infer acceptance

- **Cross-family independent cold review: NOT DONE.** Required before this
  milestone is accepted (the previous merge lacked it).
- **CI on the pushed SHA: NOT DONE.** A workflow
  (`.github/workflows/android-boundary.yml`) is added, but no run exists yet.
- **On-device gates: NOT MEASURED.** No `adb` device here: ABI load on arm64
  Android, TLS trust/entropy, real app-UID storage under
  `getNoBackupFilesDir()`, SQLite journal/WAL/SHM reopen on ext4/f2fs.
- **Android networking: unchanged.** `SO_BINDTODEVICE` is still used by
  `bound_transport.rs`/`capped_purchase.rs`; no `android_setsocknetwork` seam,
  no captured `Network`, so cellular-default-on-captive-Wi-Fi remains possible.
- **Hardened purchase controller is not wired to any UI or executable.** The
  Android service still only calls `describe`; `tollgate-agent` (Linux) still
  uses legacy `run_once`/`run_recorded`/`FileWallet`, not
  `capped_purchase::purchase_once`. Nothing calls `purchase_once` outside tests.
- `offline_policy.rs` still hardcodes `offline-simulation`; no durable production
  intent context, no fee metadata pinning, no enrollment/consent flow.
- No real module (`tollgate-module-basic-go`), mint, QEMU VM or QEMU networking
  was touched. Labgrid coordinator discovery is being handled elsewhere; shared
  QEMU/archy-vm ownership requires user handover.

## Build/CI caveats (real errors vs environment)

- The first two attempts to run the cdk+persistence Android library build timed
  out at the tool's foreground limit (420 s / 180 s) — harness limits, not build
  errors. The same script was then run in the background; its log is
  `android-hardened-build.log`. Outcome is stated below from that log verbatim.
- The host was heavily loaded by unrelated Rust builds from other worktrees
  (observed `spilman-sidecar-target` compiles), which is what stretched these
  builds. Any timing in these logs is not a CI-parity measurement.
- Ledger/vault storage tests need a private ancestor: `$HOME` is mode 0775, so
  `TMPDIR=$HOME/...` is refused by design. Tests were run under
  `umask 077` with `TMPDIR=/var/lib/tollgate-client-milestone-tests`
  (root-owned 755 ancestors, test dir 700, ext4). A refusal in a home-rooted
  TMPDIR reproduces on unmodified main and is an environmental refusal.

## Remaining integration seams (next worker)

1. Cross-family cold review of this diff; then CI green on the pushed SHA.
2. Android socket seam: injectable socket creation + `android_setsocknetwork`
   bound to a captured `Network` (keep Linux `SO_BINDTODEVICE` intact), shared by
   advert GET, session GET, POST and post-grant probes; network loss before send
   refuses, after possible write is ambiguous.
3. Storage: measure the app-UID ancestry under `getNoBackupFilesDir()`; narrow
   Android platform-root trust policy if the existing checks reject it — do not
   whitelist broadly or weaken Linux checks.
4. Production intent context: replace `offline-simulation`, persist intent
   before scheduling, bind offer/mint/unit/endpoint/network/policy version/
   installation identity, validate at the send boundary.
5. Mint keyset/`input_fee_ppk` metadata trust and full-debit bounds; typed
   serde JNI for the real purchase path (unknown/duplicate fields rejected).
6. Wire `purchase_once` into (a) the Android activity/service behind explicit
   funding + caps + one-time consent, (b) `tollgate-agent`, and prove the shipped
   binary/APK actually calls the controller (not merely that the library works).
7. Device + QEMU acceptance: reserve via labgrid, run
   `tollgate-module-basic-go`, one durable intent, independent `/session-state`
   and internet-delivery evidence, force-stop/replay with zero extra POSTs.
8. Only then: opt-in auto-buy. Autopay stays disabled until then.

## Acceptance definition (user)

Phone AND computer seamlessly buy upstream internet from
`tollgate-module-basic-go`, proven against real QEMU infrastructure (labgrid
reservation required). Installable artifacts are published only after verified
production readiness; the existing discovery-only APK
(`65c322d5…a0277`) remains unqualified for paid usage. No FIPS, no channels, no
real funds without an authorized budget.
