# Why the phone cannot complete a purchase from the emulator (definitive)

Written 2026-11-03 after a full day of attempts. This is the root cause of every
"paid-but-not-granted" on the phone side, and it is **not** a client defect.

## The finding

`tollgate-module-basic-go` opens the gate with `ndsctl auth <mac>`, where `<mac>`
is **the MAC address of the client that delivered the token**. nodogsplash only
accepts `auth` for a client it has seen on its managed interface (`br-lan`).

A TCP relay (socat / `adb reverse` / `10.0.2.2` port forwarding) terminates the
connection in the **host's** network stack, so the module sees the host bridge's
MAC (`ba:79:fa:0f:60:3d`), not the phone's. nodogsplash has no such client on
`br-lan`, so every gate call fails:

```
ndsctl auth failed, retrying (NoDogSplash may not have registered the client yet)
  attempt=4 error="exit status 1" mac_address="ba:79:fa:0f:60:3d"
ERROR: a PAID purchase could not be granted: client ba:79:fa:0f:60:3d …
  no allotment was applied (it was rolled back), the purchase is NOT complete
```

93 such failures accumulated in one session. The module is behaving correctly —
it refuses to claim access it could not apply, and rolls the allotment back.

## What this means

- **Identity is the socket's source MAC.** Any rig that puts a NAT, proxy, relay
  or `adb reverse` between client and gate cannot produce a grant, by design.
  This matches the project's own warning: "Identity is taken from the socket, so
  the token binds to the MAC of the connection that delivered it."
- The earlier `ACCESS GRANTED` on the phone (2026-11-02) happened because that
  run's `ndsctl` was the repo's **recording stub**, which accepts any MAC. It was
  real at the protocol layer and meaningless at the enforcement layer.
- The computer run succeeded because the client was a **network namespace cabled
  to the router's own bridge** with the module seeing its true MAC.

## Why the emulator cannot be fixed into this

Android's emulator gives the guest a slirp user-mode NIC. QEMU passthrough
(`-qemu -netdev tap,...`) does create a second guest NIC on the lab bridge
(`eth1`, MAC `02:00:00:00:00:50`), but Android's `netd` only routes for
interfaces it manages. A hand-added address works for raw sockets from `adb
shell` (the advert is reachable) yet **apps cannot use it**: there is no
`Network`/`NetworkAgent` for it, `ip route add default` returns "Network is
unreachable", and the framework never binds the app's sockets to it.

So a phone-side end-to-end purchase needs either:

1. **A physical handset** joined to the router's WLAN/br-lan (the real answer), or
2. A router VM whose `br-lan` is the **host's own LAN** that the phone is already
   on — i.e. give the VM a bridged NIC on the real network and let a physical
   phone reach it at the VM's LAN address. This requires a real phone too.
3. An emulator image with a routed (not bridged) L2 — not available here.

## Consequence for the milestone

| Dimension | Status |
|---|---|
| Computer buys + gets internet, supported stack, no shims | **verified** (2 distinct MACs) |
| Phone discovery/pricing against real module | **verified** |
| Phone payment reaches the real module and settles | **verified** (mint shows PAID) |
| Phone grant + delivered internet | **not verified** — blocked at L2 identity |
| Phone on a physical handset | **not attempted** |

The phone half is therefore **unproven for delivery**, and the remaining gate is
hardware, not code.
