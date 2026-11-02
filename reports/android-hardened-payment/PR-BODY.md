**Work-in-progress consolidated integration PR for Android + Linux hardened payment
against `tollgate-module-basic-go`. Do not merge: this is milestone 1 only.**

Base: live `main` `e49dcec`. Design scope:
`/home/c08r4d0r/reports/reviews/android-hardened-payment-scope.md` (stages 1–4
continued by operator instruction). Accompanying evidence lives in
`reports/android-hardened-payment/REPORT.md` and `PROGRESS.md`.

## Landed in this milestone (verified)

- **Legacy JNI bypass closed.** `Core.buy` is an ABI-compatible tombstone:
  fixed `{"state":"not-attempted","code":"payment-integration-unavailable"}`, no
  argument is read, no storage opened, no socket. RED first with a real host JVM
  against a recording loopback gate: the pre-fix library actually sent
  `['GET','GET','POST','GET','POST']` with a dummy token. GREEN: exactly one
  discovery `GET`, zero POSTs, no journal file created — also against a
  `--features android-hardened` (cdk + persistence) build.
- **Typed JNI JSON.** serde models for refusal/caps/error, integer
  `spent_today`; caps/error payloads round-trip through a real JSON parser
  including quotes, backslashes and newlines.
- **Explicit hardened feature.** `android-hardened = ["android","cdk","persistence"]`;
  the test harness feature is never implied. `android` now keeps the discovery
  JNI free of C dependencies.
- **Build contract.** New `packaging/android/build-native.sh`
  (`--lib --locked`, NDK r26, API 26, both ABIs, per-target `CC_*`/`AR_*` for
  bundled SQLite + ring, rejects `nonproduction-test-harness`);
  `build-apk.sh` copies from the derived `CARGO_TARGET_DIR` instead of a
  hardcoded `target/`.
- **Native build measured.** Both `aarch64-linux-android` and
  `x86_64-linux-android` `android-hardened` libraries built; each ELF links only
  `libdl.so`/`libc.so`, exports exactly the three JNI symbols and zero
  `capped_purchase`/`send_validated` symbols (`android-libs.txt`).
- **Host suite.** `cargo test --locked --features android-hardened` under a
  private ext4 temp root: 24 binaries, 180 passed / 0 failed.
- CI workflow (`.github/workflows/android-boundary.yml`) covering the above.

## Explicitly NOT done — do not read this as acceptance

- Hardened purchase controller is **not wired** to any UI or executable: the
  service still only calls `describe`; `tollgate-agent` still uses legacy
  `run_once`/`run_recorded`/`FileWallet`; nothing outside tests calls
  `purchase_once`.
- Android `Network` binding (`android_setsocknetwork`), app-UID storage
  measurements, production intent context (still `offline-simulation`), fee
  metadata pinning, enrollment/consent, unknown/duplicate-field rejection on the
  real purchase path.
- No device run, no module-basic-go run, no QEMU/labgrid acceptance, no real
  funds. Autopay stays disabled.
- Cross-family independent cold review and a green CI run on the final SHA are
  still owed.
- No installable artifacts are published: the existing APK is discovery-only and
  unqualified for paid usage.

## Remaining seams (next worker)

See "Remaining integration seams" in `reports/android-hardened-payment/REPORT.md`:
socket seam, storage trust policy measurement, production intent, metadata/fee
bounds, UI + CLI controller wiring with proof that the shipped APK/binary calls
it, then reserved QEMU/module-basic-go single-purchase acceptance (session +
independent internet evidence, replay with zero extra POSTs), and only then
opt-in auto-buy.

Acceptance target: phone AND computer seamlessly buy upstream internet from
`tollgate-module-basic-go`, proven on real QEMU infrastructure under labgrid
reservation. No FIPS, no channels, no merge in this milestone.
