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

- Wire change to note for UI authors: `capsFor` now emits `spent_today` as an
  integer (`0`) via serde instead of the old hand-formatted `0.000`. Same JSON
  number, integer spelling; no consumer here parses it yet.
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


## Cross-family cold review

Pinned `worker-reviewer-glm` profile (`tier/review-glm`), no tools, diff-only brief,
author here is not GLM — verdict **APPROVED**, summary: no JNI path to a payment
POST or to `FileWallet`/`run_once`/`FileJournal` remains in either feature build;
tombstone reads no arguments or storage; feature composition correct. Six minor
findings: (a) tombstone comment said "disabled even in android-hardened" while the
symbol is exported — fixed to say refusal, not absent; (b) hardcoded NDK pin had
no existence check — now fails with a clear message; (c) CI does not build either
Android ABI, so the both-ABI claim rests on one-off local evidence; (d)
`tests/android_build_test.py` asserts script text, not build behaviour; (e) the
zero-POST guarantee is exercised against one loopback response shape only; (f)
`capsFor.spent_today` integer spelling — documented above. Findings (c)–(e) are
open and named in "Remaining integration seams".
Note on independence: this is a cross-*family* review, not an independent
maintainer approval; the reviewing profile runs under the same fleet identity.

## Not verified — do not infer acceptance
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

1. Add an Android ABI build job to CI (cross-review finding c) so the both-ABI
   claim is continuously verified, not one-off. Widen the JNI boundary test to
   error/refusal response shapes and a second feature build (finding e), and make
   the packaging assertion behavioural rather than substring-based (finding d).
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

---

# Milestone 2, part A — the injectable network-binding seam

Branch `pr/android-network-seam`, based on `4499b49` (live main after PR #3).
Worktree `/home/c08r4d0r/worktrees/t_be8d64b8`. Nothing merged; no funds, no mint, no
router/QEMU/lab mutation, no FIPS edits, autopay still absent.

## The bug this closes

`BoundVerifiedBytesTransport` and `SessionReader` each built their own socket and called
`SO_BINDTODEVICE`. That is a Linux privilege model: a normal Android app cannot rely on
it. The shell selected a `Network`, dropped the object, and the kernel was then free to
route the payment POST over cellular while the phone sat on captive Wi-Fi — with the
advert `GET` and the session `GET` free to leave from yet another network. Four request
paths, four independent decisions about where the bytes went.

## The fix

`src/net_bind.rs` is now the only place a gate socket is created or attached.

| Path | Before | Now |
| --- | --- | --- |
| advert `GET` | `http::request`, unbound | `describe_gate_bound` → `BoundGet` |
| session-state `GET` | own `SO_BINDTODEVICE` socket | `SessionReader` → `connect_bound` |
| payment `POST` | own `SO_BINDTODEVICE` socket | `BoundVerifiedBytesTransport` → `connect_bound` |
| post-grant probe | session reader again | same `SessionReader` |

`connect_bound` does create → attach → source-bind → connect → verify local **and** peer
endpoints, with the same `Limits` (connect/read/write/total, header/body caps) as before.
`NetworkSelection` carries the caller's `NetworkContext` plus a `dyn NetworkBinder`, and
`PurchaseRequest::selection: Option<NetworkSelection>` is the single injection point:
`None` means the platform default (Linux `DeviceBinder`, Android `NoNetwork`).

## Fail-closed properties, and how each is proved

Evidence is the **server's** observation from a loopback listener in
`tests/network_seam.rs`; a client return value alone is never treated as proof.

| Property | Test | Observed |
| --- | --- | --- |
| one seam, four paths | `the_same_seam_carries_advert_get_session_get_post_and_probe` | 4 connections; methods `GET /`, `GET /session-state`, `POST /`, `GET /session-state`; injected binder saw all 4 |
| wrong network ⇒ nothing sent | `a_refused_network_bind_sends_nothing_on_any_path` | 0 connections, 0 POST bodies, typed refusal on each path |
| wrong network at the controller | `a_wrong_network_selection_sends_zero_posts_and_owns_no_exposure` (`tests/capped_purchase.rs`) | 0 connections, 0 POSTs, `Unresolved`, ledger owns no exposure |
| loss before the write refuses | `a_network_lost_before_the_write_boundary_refuses_the_post` | connection made, **0 POST bodies**, `RefusedBeforeSend` |
| loss after the write is ambiguous | `a_network_lost_after_the_write_is_ambiguous` | 1 POST body at the gate, `Ambiguous`, `may_have_been_sent()` |
| a lost link is not an empty session | `a_network_lost_before_the_read_is_typed_not_an_empty_session` | before the write: 0 bytes; after: `NetworkLost`, never `Inactive` |
| advert path prices over the seam | `the_advert_get_path_prices_the_body_and_refuses_without_a_network` | priced from the fetched body; 0 connections when the binder refuses |
| Android default refuses everything | `no_injected_android_network_refuses_every_path` | advert GET + session GET + POST all refuse, 0 connections |
| unspecified handle rejected | `the_android_binder_refuses_unspecified_and_fails_closed_on_host` | `handle == 0` rejected; on host the binder fails closed |
| Linux behaviour unchanged | `the_linux_default_selection_still_binds_the_device` | still binds `lo`, still fails closed on a missing interface |

## Gates

- **Gate 1 (RED).** `reports/android-hardened-payment/milestone-2-seam-red.log`:
  `unresolved import tollgate_client_core::net_bind`.
  `milestone-2-red.log` covers the second wave (production wiring stashed, tests kept):
  `cannot find net_bind`, `cannot find describe_gate_bound`.
  `milestone-2-legacy-gap-red.log` covers the independent-audit finding: the legacy
  discovery `GET` reaches the gate without consulting the binder (`bind_calls 1`, not 2).
- **Gate 2 (GREEN).** `cargo test --locked --features android-hardened` under
  `umask 077; TMPDIR=/var/tmp/tollgate-private-be8d64b8` (private root, not `$HOME`):
  **192 passed, 0 failed, 0 ignored** across 25 test binaries
  (`milestone-2-tests.log`). `tests/network_seam.rs`: 11/11.
  Default features: 96 passed. Discovery-only lib
  (`--no-default-features --features android`) builds.
- **Gate 3 (docs).** `docs/network-binding-seam.md` + updated module docs in
  `src/net_bind.rs`, `src/bound_transport.rs`, `src/capped_purchase.rs`; this section.
- **Gate 4 (commits).** One concern per commit, conventional messages, clean tree.
- **Gate 5 (push).** Branch pushed to `origin`, head SHA in the handoff. No merge, no PR.

## What is still unproven — plainly

1. **The platform call on a device.** `android_setsocknetwork` is proven to *link*
   (`milestone-2-android-link.log`: `libandroid.so` in `DT_NEEDED`,
   `UND android_setsocknetwork`), and the host build is proven to fail closed because the
   symbol is absent. Whether a real `Network.getNetworkHandle()` attaches a socket to
   captive Wi-Fi while cellular is the default is **device evidence**, and no loopback
   listener can stand in for it.
2. **The Java side does not exist yet.** No `ConnectivityManager` callback, no handle
   plumbing, no `onLost → NetworkLink::mark_lost`. Until the follow-up card lands,
   `NetworkSelection::default_for` on Android is `NoNetwork`, so the Android build refuses
   every request rather than leaving one unbound. That is deliberate — the old behaviour is
   the bug.
3. **The released `.so` does not reference the symbol.** Nothing in the cdylib's exported
   surface reaches the binder yet, so rustc drops the extern block and no `-landroid` is
   emitted. The link proof therefore uses a throwaway probe crate; the follow-up card only
   has to make the code reachable.
4. **Response-path attribution.** The seam rechecks the selection around the write, but
   "the response arrived over the bound network" is still an inference from a verified
   `peer_addr`/`local_addr` pair plus pre/post link checks, not a per-packet measurement.
5. **Real payment.** No router, mint, QEMU or funds appear in any test here.
6. **The legacy discovery path is unbound.** `Core.describe` →
   `runner::describe_gate` → `http::request` still opens a plain `TcpStream` with
   no binder, so the Android shell's *discovery* `GET` can leave over the default
   route. Only the hardened purchase path is on the seam. Claims in this report
   about "every request path" mean every **hardened** request path.
