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

## Not yet

- No auto-buy from the UI (the core supports it; the screen only prices).
- No `WifiNetworkSuggestion`: Android 10+ forbids an ordinary app from joining a
  network silently, so "auto-join then auto-buy" needs the OS-mediated suggestion
  or device-owner provisioning, and must be honest about the prompt.
- No background service, so nothing happens while the screen is off.
