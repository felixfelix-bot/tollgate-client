# Android buy path: interoperability with tollgate-module-basic-go

This records what was actually run, on 2026-11-02, and what it does and does not
prove. It supersedes the "discovery only" section of
[android-verification.md](android-verification.md) for the buy path.

## What changed

The Android shell could price an offer but had **no way to buy access**. This
adds the Lightning lane to the shell:

- `src/shell_json.rs` — the JSON contract the Java side parses
  (`request_invoice_json`, `poll_invoice_json`), with the escaping that keeps a
  remote `error` string from breaking the object it sits in.
- `src/jni.rs` — `Core.requestInvoice(gateUrl, amountSat, mintUrl, timeoutMs)`
  and `Core.pollInvoice(gateUrl, quote, timeoutMs)`.
- `MainActivity` — a **Buy access (Lightning)** button that asks the gate for a
  quote, shows the BOLT11, then polls the *same* quote as **Check payment**.

The token lane is untouched: automatic spending is still disabled, and a stored
note is still never spent automatically. `Core.buy` remains experimental.

## Why Lightning and not the token lane

The token lane needs a Cashu wallet on the phone: a seed, durable spend state,
and NUT-07 reconciliation for ambiguous sends. Those are exactly the parts this
client does not have, and shipping them half-built is how a customer pays twice.
The Lightning lane needs none of it — the gate creates the quote, the customer
pays it in whatever wallet they already have, and the gate opens access itself.
Nothing of value is stored on the device.

`access_granted: true` is the only field treated as success. A
`paid-but-not-granted` answer sets `retry: true` and the UI says "tap again";
it never requests a second invoice.

## The run

Host: the module built from source (`tg-wrt`, Go 1.26.10, CGO disabled),
started with `TOLLGATE_TEST_CONFIG_DIR` pointing at a config whose accepted mint
is the local fakewallet node. `ndsctl` was the repo's own
`tests/cloud-lab/fake-ndsctl.sh` on PATH, logging every gate call. Client
identity was seeded in `/tmp/dhcp.leases` as `02:00:00:00:00:20`, which is what
the module resolves from the socket.

Device: AVD `tollgate`, `system-images;android-34;google_apis;x86_64`, headless,
1536 MB, KVM. The phone reaches the module at `10.0.2.2:2121` — its default
gateway.

### Desktop client (control)

```
$ ./target/release/tollgate-agent --lightning --gate http://127.0.0.1:2121 \
      --amount 2 --mint-url http://127.0.0.1:3338/ --poll-ms 500
note: --amount is ignored; the price comes from the advert
PAY THIS INVOICE (1 sat): lnbc10n1p4wjz...
PROBE gate=http://127.0.0.1:2121 offers=1 \
  decision=Buy { gateway_id: "http://127.0.0.1:2121", steps: 1, cost: 1.0, unit: "sat" }
RESULT state=granted allotment=22020096 metric=bytes
```

### Phone

Tapping **Buy access (Lightning)** on the emulator:

1. The button became **Check payment** — `Core.requestInvoice` returned a quote
   from the real module (a JNI symbol that failed to resolve would have thrown
   `UnsatisfiedLinkError` and left the button alone).
2. The module's `ndsctl` log gained a third line, `AUTH 02:00:00:00:00:20`, at a
   timestamp after the desktop run — the phone's purchase opened the gate.
3. Tapping **Check payment** produced **ACCESS GRANTED** on the device screen,
   and the button reset to **Buy access (Lightning)**.
4. `/session-state` on the module reported `{"state":"active"}`.

The mint log shows the matching quote going `UNPAID → PAID → ISSUED`, so this was
a settled ecash issuance, not a mocked grant.

## What this proves

- The Android shell buys from **tollgate-module-basic-go** over its documented
  wire contract, with the module's own advertisement, pricing and gate-opening
  logic in the path.
- The grant is real: the gate opened for the phone's identity and the module
  reported the session active.
- The two-step UI cannot silently double-charge: one quote is held and polled.

## What this does NOT prove

- **No real router.** The module ran on the host; `ndsctl` was the repo's fake.
  nftables, NoDogSplash, and actual packet enforcement were not exercised. A
  QEMU OpenWrt VM run is still owed.
- **No real mint, no real Lightning.** The mint is `cdk-mintd` with the
  `fakewallet` backend, which settles invoices for free. A production mint and a
  production Lightning payment are untested.
- **No real phone.** Emulator only, x86_64.
- The mint now comes from **the gate's own advertisement** (`mint_choice`,
  7 tests), with an optional user override that is honoured only if the gate
  advertises it. Verified below with the field left blank.
- **No `WifiNetworkSuggestion`.** The customer still joins the Wi-Fi themselves;
  Android forbids an ordinary app from doing it silently.
- The APK is **debug-signed**.

Verdict: interoperable with tollgate-module-basic-go on the buy path, verified on
emulator against a fakewallet mint. **Not production ready** — the mint setting
and a real-router run are the remaining release gates.

## Mint selection (added after the first run)

The first build carried `http://127.0.0.1:3338/` as a hardcoded mint default,
which meant the phone could only ever buy on the lab network. That is now gone:

- `describe` reports `"mints":[…]` — every mint the gate advertises, in advert
  order, de-duplicated.
- `mint_choice::choose_mint(override, advertised)` decides: a non-empty override
  wins **only if the gate advertises it** (asking a gate for a quote at a mint
  it does not accept is a guaranteed refusal, so an unhonoured override is an
  error, not a preference); with no override the first advertised mint is used;
  no override and no advertised mint is an error, never a guess.
- The activity has a **mint URL** field. Blank means "ask the gateway", which is
  the intended default.

Re-verified with the field cleared (`pm clear`) and the app reinstalled: tapping
*Buy access* → **Check payment** (quote obtained) → gate logged a new
`AUTH 02:00:00:00:00:20` → **ACCESS GRANTED** on screen → `/session-state`
`active`. No mint was configured at any point.
