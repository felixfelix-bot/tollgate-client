# Phone → real tollgate-module-basic-go buy: VERIFIED (emulator + QEMU router)

Run 2026-11-03 CET. Client main `c3cd2f8` (PR #4 merged). No shared lab touched:
own QEMU overlay, own TAP `tap-tg0`, own relay on 127.0.0.1:2121.

## Rig

- Router: OpenWrt 24.10.1 x86-64 in QEMU/KVM, overlay on
  `tollgate-virtual-lab-clean/images/openwrt-base.qcow2`, TAP NIC `tap-tg0`
  (host 10.99.99.2, guest br-lan renumbered to 10.99.99.1 to avoid colliding
  with the host's real LAN 192.168.1.0/24), plus a slirp NIC for WAN/mint.
- Module: built from `~/repos/tollgate-module-basic-go` @ `eb12d7f8`,
  `CGO_ENABLED=0 GOOS=linux GOARCH=amd64`, sha256 `2e4882f0f9f8ca8d…`,
  run under procd (`/etc/init.d/tollgate`) with `PATH=/opt/tg/bin:…` and
  `NDSCTL_LOG=/tmp/ndsctl.log`.
- Mint: host `cdk-mintd/0.18.3` at `:3338`, `fakewallet` backend (free settle).
  Reached from the guest as `http://10.0.2.2:3338/` — that is the mint the
  advert advertises.
- Phone: AVD `tollgate`, android-34 x86_64, API 34; APK `dev.tollgate.client`
  v0.1.0, sha256 `dfcfebed860924f1b87f7c8891e27dfeb352dcfbf1282ab905506092b7f8cde1`,
  debug-signed (CN=TollGate Debug).
- `ndsctl`: the repo's recording stub (`tests/cloud-lab/fake-ndsctl.sh`) at
  `/opt/tg/bin/ndsctl`. **NoDogSplash/nftables are NOT in the path.**

## Observed

1. **Discovery, from the phone**, against the real module's signed advert:
   `{"kind":10021,…,"tags":[["metric","bytes"],["step_size","22020096"],
   ["tips","1","2"],["price_per_step","cashu","1","sat",
   "http://10.0.2.2:3338/","1"]]}`
   UI: `gateway 10.0.2.2`, price_per_step 1 sat, 1 step, cost 1.000,
   `mints:["http://10.0.2.2:3338/"]`, decision `Buy`.
2. Tap **Buy access (Lightning)** → button became **Check payment**; the UI
   showed `pay this invoice (1 sat): lnbc10n1p4wj7a5dqqpp5z4zge5wjkq4xpssup58jy4f5tvc42egzx6sj9t87fh9ada7jtt0q…`.
   That BOLT11 came from the real module, which relayed it from the mint.
3. First **Check payment** → `{"state":"paid-but-not-granted","access_granted":false,"retry":true,…}`.
   Real defect found and fixed in the rig, not the app: procd started the module
   without `/opt/tg/bin` on `PATH`, so its `ndsctl` call failed and the gate was
   not opened. With `PATH` set, the module logged `AUTH ae:3a:8c:a6:af:6e`.
4. After the fix, **Check payment** → **ACCESS GRANTED** on the device:
   `{"state":"ISSUED","access_granted":true,"allotment":22020096,"metric":"bytes"}`.
5. Independent module evidence, read from the router itself:
   - `/tmp/ndsctl.log` → `2026-11-03T06:15:25Z AUTH ae:3a:8c:a6:af:6e`
   - `GET /whoami` → `mac=ae:3a:8c:a6:af:6e`
   - `GET /session-state` → `{"status":1,"mac":"ae:3a:8c:a6:af:6e","state":"active"}`
   - module log: valve polling `state=Authenticated` for that MAC.

So: quote → mint settle → gate AUTH → session active, driven entirely by the
phone app against the real Go module running in a router VM.

## What this does NOT prove

- **No packet enforcement.** `ndsctl` is a recording stub: opennds and the fw4
  rules were never in the path, so "internet actually flows after payment" is
  unproven. This is the outstanding acceptance gate.
- **No real mint / no real Lightning.** `fakewallet` settles for free.
- **No internet delivered to the phone**: the emulator reaches the gate over a
  host relay (slirp), it is not attached to the router's br-lan, so its traffic
  does not traverse the VM.
- Emulator only, x86_64; no physical handset.
- APK is debug-signed.
