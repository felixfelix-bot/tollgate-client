# Android acceptance run

Device: AVD `tollgate`, `system-images;android-34;google_apis;x86_64`, Android 14,
headless (`-no-window -gpu swiftshader_indirect`, KVM).
Gate: a stand-in serving the v1 advert on the host at `:2121`, reachable from the
emulator as `10.0.2.2` — i.e. exactly the phone's default gateway.

## Steps

```
avdmanager create avd -n tollgate -k "system-images;android-34;google_apis;x86_64"
emulator -avd tollgate -no-window -no-snapshot -gpu swiftshader_indirect
adb install -r tollgate-android.apk      # Success in 1662 ms
adb shell am start -n dev.tollgate.client/.MainActivity
```

## Observed

The activity displayed, no `UnsatisfiedLinkError`, no crash. On screen:

```
gateway 10.0.2.2

{"gate":"http://10.0.2.2:2121/",
 "offers":[{"unit":"sat",
            "price_per_step":3,
            "steps":1,
            "cost":3.000,
            "cost_per_minute":3.000,
            "quality":0.500,
            "p":0.500}],
 "decision":"Prompt { reason: \"http://10.0.2.2:2121/ costs 3.0000 sat/minute
             which is above the per-minute cap 2.0000\" }"}
```

Every field came from the gate: the phone read its default route, built the URL,
crossed JNI into Rust, parsed the advert, priced it and applied the cap. The
decision is `Prompt`, not `Buy`, because 3 sat/min exceeds the default 2 — the
client declined to spend on its own, on a device.

Independent confirmation that the phone really made the request: the gate logged
one `GET /` per app launch and no more (1 for a manual curl, +1 launch, +1 after
`am force-stop` and relaunch).

## Historical run limitations

- No auto-buy from the UI (the core supports it; the screen only prices).
- No `WifiNetworkSuggestion`: Android 10+ forbids an ordinary app from joining a
  network silently, so "auto-join then auto-buy" needs the OS-mediated suggestion
  or device-owner provisioning, and must be honest about the prompt.
- At the time of this historical run, no background service was present.

## Foreground safety fix (current branch)

The service is now discovery-only. It declares both FOREGROUND_SERVICE and
FOREGROUND_SERVICE_DATA_SYNC for its dataSync type and target SDK 34. A runtime
foreground-start refusal calls stopSelf; a subsequently delivered onStartCommand
returns START_NOT_STICKY without starting a worker. Fatal JVM Errors are not swallowed.

Automatic spending is disabled entirely, not hidden behind an opt-in. The old loop
used a new `svc-<minute>` ID every tick and cleared the token only on a grant. An
ambiguous response therefore retained the same note but bypassed journal
per-attempt deduplication on the next minute or process restart. FileWallet cannot
perform NUT-07 reconciliation. The JNI caps reset `spent_today` to zero on every
call; the advertised daily budget is not a durable ledger. Core.buy also constructs
JSON by hand (including Debug output), which is not a reliable parsing contract.
These payment-path defects remain, but the Android watcher no longer calls it.
Existing saved notes are left untouched, never automatically spent. Do not store
real funds in this prototype's SharedPreferences.

Verified on this branch (no funds, routers, or keys used):
- `python3 tests/android_service_test.py`: 2 tests passed. Host JVM compiles and
  runs the actual service against Android API doubles: SecurityException and
  platform refusal stop without a worker; successful startup creates one worker;
  destroy terminates it; repeated ticks and fresh instances with a dummy stored
  token and Buy advert call Core.buy zero times.
- All Android Java sources compile against the installed Android 34 SDK jar.
- `cargo test`: 61 tests passed, 0 failed.
- `cargo test --no-default-features --features android`: 61 passed, 0 failed.

The JVM regression is not an emulator instrumentation test. No newly signed APK
was produced or installed in this fix (key handling is outside scope). Prior mock
gate POST/grant observations prove only a mock transaction, not Cashu settlement,
persistent budget enforcement, or network access. Device verification of this
exact branch, notification permission UX, newer-Android foreground-service time
limits, and real payment/reconciliation remain release gates. Not production ready.
