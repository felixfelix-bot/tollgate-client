# TollGate client — dogfood build, 2026-11-03

Installable artifacts from the interoperable build. Base commit
`c3cd2f8` (client `main`), tested against
`tollgate-module-basic-go` @ `eb12d7f8` in an OpenWrt 24.10.1 QEMU VM with
real openNDS enforcement.

## Phone (Android)

**https://blossom2.orangesync.tech/dfcfebed860924f1b87f7c8891e27dfeb352dcfbf1282ab905506092b7f8cde1.apk**

- 598,660 B · sha256 `dfcfebed860924f1b87f7c8891e27dfeb352dcfbf1282ab905506092b7f8cde1`
- package `dev.tollgate.client` 0.1.0, minSdk 26, targetSdk 34
- **debug-signed** (CN=TollGate Debug) — enable "unknown sources" to install
- Both ABIs included (arm64-v8a, x86_64)

Install: `adb install tollgate-android.apk`, or copy to the phone and tap.

## Computer (Linux x86-64)

**https://blossom2.orangesync.tech/b55c3d3d1f0b6dcad10e411dcc089a4fc1566a2010e6463770426787fe651eec.xz**

- xz-compressed, 5,061,736 B → 14,471,384 B binary
- sha256 of the `.xz` `b55c3d3d1f0b6dcad10e411dcc089a4fc1566a2010e6463770426787fe651eec`
- built `--release --features cdk,persistence`

```sh
curl -LO https://blossom2.orangesync.tech/b55c3d3d1f0b6dcad10e411dcc089a4fc1566a2010e6463770426787fe651eec.xz
xz -dc b55c3d3d….xz > tollgate-agent && chmod +x tollgate-agent
./tollgate-agent --lightning --gate http://<router>:2121/
```

## What was verified (2026-11-03)

- **Computer**: `tollgate-agent --lightning` from a client cabled to the router's
  own L2 → `RESULT state=granted allotment=22020096 metric=bytes`; module reported
  `/session-state active`; `ndsctl` showed one authenticated client it owned; the
  same client then fetched `https://one.one.one.one/cdn-cgi/trace` (`colo=FRA`)
  and the GitHub API **with TLS certificate validation on**.
- **Phone**: emulator bought from the same module → `ACCESS GRANTED`,
  `{"state":"ISSUED","access_granted":true,"allotment":22020096}`.

## Deployment facts you need on a real router

1. **Port 2121 must be reachable before payment.** openNDS's `ndsRTR` input chain
   (priority `-100`) rejects it before the module's `30-backend-firewall.nft`
   accepts, so pre-auth clients cannot fetch the advert and cannot buy. Add 2121
   to the openNDS walled garden (and confirm the module ships that exemption).
2. **`dnsmasq-full`** is required by openNDS on 24.10; plain `dnsmasq` fails its
   nftset setup.
3. **A client needs a DHCP lease** before openNDS recognises it; without one the
   module correctly rolls back and reports the purchase incomplete.

## Not production-ready

- Mint is `cdk-mintd` with the **`fakewallet`** backend — settles free, no real sats.
- **Debug-signed** APK.
- Phone was verified on the **emulator**; the emulator reached the gate through a
  host relay, so openNDS was not in the phone's packet path.
- The token/ecash lane is not wired to the UI; the Lightning lane is the only buy
  path in this build. Automatic spending is disabled.
