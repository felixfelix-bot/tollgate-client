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

`umask 077`, `TMPDIR=/var/lib/tollgate-client-milestone-tests` (private ext4 dir,
root-owned 755 ancestors), `cargo test --locked --features android-hardened`:
24 test binaries, **180 passed / 0 failed**, including `capped_purchase`,
`offline_policy`, `boundary_store`, `ledger_storage`. Log:
`reports/android-hardened-payment/host-full-tests.log`. This is a host suite on
Linux: it does not prove Android behaviour.

### 5. Android native build (both ABIs measured)

`TOLLGATE_ANDROID_FEATURE=android-hardened packaging/android/build-native.sh`
(`--lib --locked`, NDK 26.1.10909125, API 26). Verbatim symbol/NEEDED evidence:
`reports/android-hardened-payment/android-libs.txt`; full log
`android-hardened-build.log`.

- `aarch64-linux-android`: **built**. `libtollgate_client_core.so` is ELF 64-bit
  ARM aarch64; `DT_NEEDED` is only `libdl.so`, `libc.so` (bundled SQLite, static
  ring — no OpenSSL); exported JNI symbols are exactly `describe`, `buy`,
  `capsFor` and zero `capped_purchase`/`send_validated` symbols, i.e. the hardened
  controller is compiled in but not reachable from Java.
  SHA256 `f92cfec305644ad4fc8f59cfc776be07fe8e4a656c601a8db13de6496b046ee8`.
- `x86_64-linux-android`: **built**. Final line of the run:
  `Finished \`release\` profile [optimized] target(s) in 24m 19s`, zero `error`
  lines in the log. `libtollgate_client_core.so` is ELF 64-bit x86-64,
  2,651,944 bytes, SHA256
  `0913417403aec75370833cd35ac4f721e4d5f3e2b6d0b1a31a5428efec90b4c9`.
- Host-loading the aarch64 `.so` in a JVM is expected to fail (wrong machine) and
  did; that is not a defect. The `android-hardened` tombstone was instead proven
  by JVM/socket test against a host-built `android-hardened` debug `cdylib`
  (`jni-green-android-hardened-debug.log`), and by the aarch64 `.so` symbol table.
- The `--no-default-features` cross `cargo check` for both Android targets did
  not complete before scope freeze (blocked behind the shared build-directory
  lock). Not a red result — an unfinished one. Re-run:
  `CARGO_TARGET_DIR=target-minimal cargo check --locked --lib
  --no-default-features --target aarch64-linux-android` (and `x86_64`).


## Not verified — do not infer acceptance

- **Cross-family independent cold review: NOT DONE.** Required before this
  milestone is accepted (the previous merge lacked it).
- **CI on the pushed SHA: GREEN now; earlier run was RED for a real reason.** Run
  41528425939 (`4fb1be8`) failed because `cdk-signatory 0.18.3`'s build script
  needs `protoc`, absent on the runner: `Could not find protoc ... apt-get
  install protobuf-compiler`. A real build dependency of the hardened feature
  (present on this box, which is why local configuration succeeded), not a test
  failure. The workflow now installs `protobuf-compiler`; runs for `d55db45` and
  `b5dc34b` both completed `success` (host hardened suite, full-feature JVM
  refusal, discovery JNI, service regression, build contract, `cargo fmt --check`).
  This is Linux CI, not an Android device.
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
