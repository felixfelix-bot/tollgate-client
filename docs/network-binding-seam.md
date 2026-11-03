# The injectable network-binding seam

`net_bind` is the **one** place in this crate where a **hardened** gate socket is
attached to a network. Advert `GET` (via `runner::describe_gate_bound`),
session-state `GET`, the payment `POST` and the post-grant session probe all
create their socket here, so no hardened request path can leave from a different
network than the others.

**Scope, precisely.** `crate::http::request` also still exists and still opens a
plain unbound `TcpStream`. It is the **legacy LAN** transport, used by
`runner::describe_gate`, `runner::run_once`, `HttpGate` and therefore the
`Core.describe` JNI export. Those call sites are deliberately unchanged in this
change and are **not** on the seam. The practical consequence today: the Android
shell's discovery `GET` is unbound; only the hardened purchase path is bound.
Rewiring the legacy discovery export onto the seam is a separate change with its
own tests — do not read this document as claiming it is already done.

Requires `cdk` + `persistence` on Linux/Android for the purchase path; the seam
itself is compiled on both targets. Lab-free: every test binds a loopback
`TcpListener` and asserts on **what the server observed**, never on the client's
own return value.

## Why

`bound_transport::send` and `SessionReader::connect` each used to build their own
socket with `SO_BINDTODEVICE`. That is a Linux privilege model: a normal Android
app cannot rely on it. The Android shell selected a `Network`, dropped the
object, and the kernel was free to route the payment over cellular while the
phone sat on captive Wi-Fi.

The seam replaces "an interface name" with "a binder that can attach a socket",
and keeps the Linux behaviour as one implementation of that interface.

## Shape

| Type | Role |
| --- | --- |
| `NetworkBinder` | `bind(&Socket)`, `link_intact()`, `generation()`, `kind()`, `lost()` |
| `NetworkSelection` | the caller's `NetworkContext` (source + interface label) plus its binder |
| `BoundSocket` | a connected socket that is on the selected network; `recheck()` asks the binder whether it still is |
| `connect_bound` | create → attach → source-bind → connect → verify local **and** peer endpoints |
| `BoundGet` | one bounded `GET` through the seam (advert read and session probe share it) |
| `DeviceBinder` | Linux `SO_BINDTODEVICE`, byte-for-byte the reviewed behaviour |
| `AndroidNetworkBinder` | `android_setsocknetwork(handle, fd)` from libandroid |
| `NetworkLink` | the liveness flag + generation for one Android `Network` |
| `NoNetwork` | fail-closed placeholder: refuses every bind |

`BindError` is mapped onto each caller's own vocabulary (`RefusalReason` for the
POST, `SessionReadError::NetworkLost` for the session reads, `GetError` for the
advert), so no caller has to invent a meaning for a binding failure.

## The rules, structurally

* **No process-wide bind.** `android_setprocnetwork` is never called. Every
  request attaches its own socket.
* **No unbound fallback.** A binder that cannot attach is an error. There is no
  branch that continues without binding.
* **No retry, no redirect.** A single `connect`, a single `write`.
* **Loss before the write refuses.** `link_intact()` is checked after connect and
  immediately before the write; a false answer is a refusal, and the server sees
  no request byte.
* **Loss after a possible write is ambiguous.** The POST returns
  `Ambiguous { NetworkGenerationLost }` / `NetworkInterfaceLost`, never
  `RefusedBeforeSend` and never `Complete`. A GET that already wrote its request
  returns `NetworkLost`, never an "inactive" session that would licence a spend.
* **A reused interface name or address is not identity.** The Android path names
  losses by generation, not by interface label.

## Linux default (unchanged)

`NetworkSelection::default_for` on Linux returns `DeviceBinder` for the context's
interface: `SO_BINDTODEVICE` plus the source bind, with both endpoints verified
and the `InterfaceBindingFailed` / `InterfaceVanished` refusals the shipped
daemon already produced. `tests/network_seam.rs::the_linux_default_selection_still_binds_the_device`
is the regression: a bound attempt connects, a missing interface still refuses
before any connection.

## Android (seam only — capture is a follow-up card)

`NetworkSelection::android_network(context, handle, link)` is the entry point.
`handle` must be `Network.getNetworkHandle()`; `0` is `NETWORK_UNSPECIFIED` and
`android_setsocknetwork` would *clear* a binding, which is exactly the unbound
fallback this seam forbids, so construction rejects it.

```rust
let link = NetworkLink::new(generation);       // from the ConnectivityManager callback
let selection = NetworkSelection::android_network(context, handle, link.clone())?;
// ... hand `selection` to BoundGet / SessionReader::with_selection /
//     PurchaseRequest::selection ...
link.mark_lost();                              // from onLost / onUnavailable
```

`NetworkLink::mark_lost` is irreversible: a replacement network is a different
generation and must be a different selection.

### What an Android build does today

Nothing in this crate captures a `Network` from Java yet, so
`NetworkSelection::default_for` on `target_os = "android"` is `NoNetwork`, and
**every** request path refuses with `NetworkBindingFailed { kind: "no-network" }`
/ `SessionReadError::NetworkLost`. That is deliberate: the current shell's
behaviour is the bug, and an unbound socket is not an acceptable default.

The APK shipped before this change is discovery-only and unqualified for paid
usage; it is unchanged by this work.

## What is still unproven

Named plainly, because a green host suite must not be read as an Android claim:

1. **The platform call on a device.** `android_setsocknetwork` is exercised on the
   host only through its *absence*: the host build has no libandroid, so the
   binder fails closed, and the test asserts exactly that. Whether a real
   `Network.getNetworkHandle()` attaches a socket to captive Wi-Fi while cellular
   is default is **device evidence**, not something a loopback listener can show.
2. **AOT/NDK linkage.** `cargo check`/`build --target aarch64-linux-android`
   passes with the `link(name = "android")` attribute; that the resulting `.so`
   resolves the symbol at load time on API 26 must be measured on the device
   (`llvm-readelf -d` for `libandroid.so` in `DT_NEEDED` is necessary, not
   sufficient).
3. **The Java side.** No `ConnectivityManager` callback, no network handle
   plumbing, no `onLost` → `NetworkLink::mark_lost` wiring exists yet. That is the
   follow-up card; this one deliberately stops at the seam and its contract.
4. **Post-grant verification of the response path.** The seam rechecks the
   selection around the write, but "the response really arrived over the bound
   network" is still an inference from a verified `peer_addr`/`local_addr` pair
   plus the pre/post link checks, not a measured per-packet observation.
5. **Real payment.** No router, mint, QEMU or funds are involved in any test in
   this document.
6. **The legacy discovery path is still unbound.** `Core.describe` →
   `runner::describe_gate` → `http::request` creates a plain `TcpStream` with no
   binder. So on Android today the *discovery* `GET` can still leave over the
   default route; only the hardened purchase path (advert read through
   `describe_gate_bound`, session reads, `POST`, post-grant probe) is attached to
   the selected network. Wiring the legacy export is the next card, and it needs
   a binder in the JNI signature — which needs the Java `Network` capture that
   does not exist yet.

## Tests (`tests/network_seam.rs`)

| Test | Server-side claim |
| --- | --- |
| `the_same_seam_carries_advert_get_session_get_post_and_probe` | 4 connections, methods `GET /`, `GET /session-state`, `POST /`, `GET /session-state`, and the injected binder saw all 4 |
| `a_refused_network_bind_sends_nothing_on_any_path` | 0 connections, 0 POST bodies; binder consulted 3× and every path returned a typed refusal |
| `a_network_lost_before_the_write_boundary_refuses_the_post` | 1 connection (already made), **0 POST bodies**, `RefusedBeforeSend` |
| `a_network_lost_after_the_write_is_ambiguous` | 1 POST body delivered, `Ambiguous`, `may_have_been_sent() == true` |
| `a_network_lost_before_the_read_is_typed_not_an_empty_session` | before the write: 0 request bytes; after the write: `NetworkLost`, never `Inactive` |
| `the_advert_get_path_prices_the_body_and_refuses_without_a_network` | advert priced from the body fetched over the seam; with a refusing binder, 0 connections |
| `no_injected_android_network_refuses_every_path` | the Android default refuses advert GET, session GET and POST; 0 connections |
| `the_android_binder_refuses_unspecified_and_fails_closed_on_host` | handle `0` rejected; host lacks libandroid so the POST refuses with 0 connections |
| `the_android_entry_point_carries_a_handle_and_its_link` | handle + link carried; `mark_lost` is irreversible |
| `the_linux_default_selection_still_binds_the_device` | the Linux default still binds the device and still fails closed on a missing interface |
| `the_legacy_discovery_path_is_still_outside_the_seam` | **pins the known gap**: both paths reach the gate (2 connections), the binder is consulted once — the legacy discovery `GET` is unbound |

`tests/capped_purchase.rs::a_wrong_network_selection_sends_zero_posts_and_owns_no_exposure`
is the orchestration-level counterpart: with a binder that refuses to attach,
`purchase_once` reaches the gate 0 times, sends 0 POSTs, and the ledger owns no
exposure (the baseline read failed, so nothing was committed).
