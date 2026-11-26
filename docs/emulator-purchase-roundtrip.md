# Emulator round-trip: the phone buys from a gate, and the gate says so

Device: AVD `tollgate`, `system-images;android-34;google_apis;x86_64`, headless,
KVM. Built from this tree with `packaging/android/build-apk.sh` (no Gradle).

This record exists because the previous acceptance run
([android-verification.md](android-verification.md)) stopped at *pricing*: it
showed the phone reading an advert and deciding `Buy`, but never a purchase.
Here the whole purchase is exercised, and the interesting part is that the
first two attempts **failed for a real reason on our side**, not in the app.

## The AVD had to be repaired before anything could be tested

`~/.android/avd/tollgate.avd/config.ini` was `1080x2400` with `ramSize=1536M`,
`vm.heapSize=228M`. On a headless guest SwiftShader renders 2.6 M pixels per
frame in software and synthetic taps are dropped; the emulator instance had
also died. Changed to `540x1200 @240dpi`, `ramSize=3072M`, `heapSize=512M`,
`ncore=4`. Boot then reached `sys.boot_completed=1` in ~30 s and taps land.
`wm size` confirms `Physical size: 540x1200`.

## Wire contract, read off the client and then obeyed

`src/purchase.rs::classify_response` accepts exactly two answers:

- `HTTP 200` with `kind: 1022` → `Granted { allotment, metric, device-identifier }`
- `kind: 21023` → classified by `code` through `by_code()`

Anything else is **`Ambiguous` on purpose**, and an ambiguity makes the agent
ask the wallet for NUT-07 (`is_spent`). With `runner::FileWallet` that answers
`FileWallet cannot answer NUT-07`, so the attempt ends `ambiguous` and no grant
happens. That is the designed behaviour, and it is exactly what the journal
recorded while the stand-in was answering inventively.

Also measured: `POST /` sends the **token as the raw request body**
(`cashuB…`, 18 bytes here), not a JSON envelope.

## What the stand-in answers now

`tools/gate-double.py` (in this PR) serves the advert at `/` and, on `POST /`,
answers `kind: 1022` with `allotment`, `metric`, `device-identifier`. It logs
every request with its source address so the *server* side can prove the phone
called.

## Observed, on the device

```
GET  /    from the phone             -> advert with price_per_step 2 sat
POST /    18 bytes, body cashuB…     -> {"kind":1022,"allotment":60000,…}
app journal: pending -> granted
gate log:    {"event":"grant","granted":120000,"steps":1,"token_prefix":"cashuBdemoto"}
```

Screenshot `bought.png` shows the app with the note saved and the watch
started. `dumpsys activity activities` lists
`dev.tollgate.client/.MainActivity` as `topResumedActivity`, and
`pidof dev.tollgate.client` stayed alive across the run — no crash, no
`UnsatisfiedLinkError`, no `FATAL EXCEPTION` in logcat.

Two benign warnings appear and are not defects: `Unexpected CPU variant for
x86: x86_64` (ART choosing a generic variant) and a missing `base.dm`.

## What this does NOT prove

- The stand-in is **not** `fips-exitd`, and no mint verified anything
  (`verified_by_mint: false` is in every response). This is the phone's HTTP
  contract working end to end over a real network path, not a paid FIPS exit.
- No FIPS transport, no tunnel, no per-peer accounting, no revocation.
- The post-purchase notification was not visually confirmed; the service
  reaches `post(...)` but `POST_NOTIFICATIONS` is a runtime permission and the
  guest dialog was not accepted in this run.

## Reproduce

```sh
export JAVA_HOME=$HOME/.local/jdk ANDROID_SDK_ROOT=$HOME/android-sdk
sh packaging/android/build-apk.sh
adb -s emulator-5554 install -r tollgate-android.apk
python3 tools/gate-double.py --port 2121 --log /tmp/gate.jsonl &
adb -s emulator-5554 shell am start -n dev.tollgate.client/.MainActivity
# tap "Find gateway and price it", type a note, "Save note", "Start background watch"
python3 -c 'import json;[print(json.loads(l)) for l in open("/tmp/gate.jsonl")]'
adb -s emulator-5554 shell run-as dev.tollgate.client \
  cat /data/data/dev.tollgate.client/files/journal.txt
```
