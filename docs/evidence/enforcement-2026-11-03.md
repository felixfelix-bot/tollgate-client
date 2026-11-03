# Interop + enforcement: phone AND computer buy internet from a QEMU OpenWrt router

Run 2026-11-03 09:2x CET. Client main `c3cd2f8`. No shared lab infra touched: own
QEMU overlay, own TAP `tap-tg0` on own bridge `br-tg`, own netns client.

## Rig

- Router: OpenWrt 24.10.1 x86-64, QEMU/KVM, overlay on
  `tollgate-virtual-lab-clean/images/openwrt-base.qcow2`, `tap-tg0` (no slirp for
  the customer side, so no NAT shortcut).
- Real **openNDS 10.3.1** on `br-lan` + `dnsmasq-full` (stock dnsmasq cannot
  configure openNDS's nftset blocklist). The recording `ndsctl` stub is GONE.
- Module: `~/repos/tollgate-module-basic-go` @ `eb12d7f8`, `CGO_ENABLED=0`
  amd64, under procd at `/opt/tg/tg`.
- Mint: host `cdk-mintd/0.18.3`, `fakewallet` (settles free), advertised as
  `http://10.0.2.2:3338/`.
- Customer client: **network namespace `tglab`**, veth `veth-tg1` = `10.99.99.50`
  cabled to the same bridge as the router's `br-lan` (10.99.99.1). Real L2, no NAT.
  No DHCP client, so its lease is seeded in `/tmp/dhcp.leases` — openNDS needs a
  lease before it will recognise a client at all.

## Deployment facts found the hard way

These are real router facts, not client bugs:

1. **Port 2121 must be reachable pre-auth.** openNDS's `ip nds_filter ndsRTR`
   (hook input priority **-100**) rejects the payment API before the module's own
   `30-backend-firewall.nft` (inside `inet fw4`) can accept it. A pre-auth client
   cannot even fetch the advert, so it can never buy. Fixed in this rig by
   `nft insert rule ip nds_filter ndsRTR tcp dport 2121 counter accept` and by
   putting the gate IP in the openNDS walled garden.
2. **`dnsmasq-full` is required** by openNDS on 24.10; plain `dnsmasq` makes
   openNDS fail its nftset setup ("Dnsmasq reload failed").
3. **openNDS identifies clients by DHCP lease.** With no lease the module's grant
   call fails (`failed to open gate: exit status 1`) and it correctly rolls the
   allotment back and says the purchase is NOT complete. After seeding the lease,
   the same purchase granted.

## Observed

### Computer (Linux `tollgate-agent`, release, cdk+persistence)

Run inside netns `tglab`, i.e. from the address the router sees:

```
$ sudo ip netns exec tglab ./target/release/tollgate-agent --lightning \
    --gate http://10.99.99.1:2121/ --timeout-ms 8000 --max-polls 30 --poll-ms 2000
PAY THIS INVOICE (1 sat):
lnbc10n1p4wn2padqqpp5qukcumjy2gyreklm3ecfkaej6llcdkms7804d3u9fgvxftgv5zp…
PROBE gate=http://10.99.99.1:2121/ offers=1 decision=Buy { steps: 1, cost: 1.0, unit: "sat" }
RESULT state=granted allotment=22020096 metric=bytes
```

Gate, read from the router:
```
$ ndsctl status | grep -E 'Current clients|authentications since'
Client authentications since start: 1
Current clients: 1
$ curl -s http://10.99.99.1:2121/session-state
{"status":1,"mac":"2e:c5:0f:63:d2:35","state":"active"}
```

**Delivered internet, from the same netns, with TLS certificate validation on**
(a captive portal cannot produce these):

```
$ curl -sS --resolve one.one.one.one:443:1.1.1.1 https://one.one.one.one/cdn-cgi/trace
fl=608f340
h=one.one.one.one
ip=167.235.218.44
visit_scheme=https
colo=FRA
sliver=050-tier1

$ curl -sS --resolve api.github.com:443:140.82.121.6 https://api.github.com/zen
Anything added dilutes everything else.
```

DNS through the router also answered for a real name
(`github.com → 140.82.121.4` via a raw query to 10.99.99.1:53).

### Phone (emulator, earlier run, same module)

Real advert, real BOLT11 from the module, then `ACCESS GRANTED`
`{"state":"ISSUED","access_granted":true,"allotment":22020096,"metric":"bytes"}`,
and on the router `ndsctl AUTH ae:3a:8c:a6:af:6e`, `/whoami` matching,
`/session-state active`. Screenshot `docs/evidence/granted.png`.

## What this proves / does not prove

Proved: a client on the router's own L2 with real openNDS enforcement can buy from
the real module and then reach the internet with a validated TLS certificate.

Not proved:
- Real money/Lightning: `fakewallet` settles free.
- Physical handset: emulator only, x86_64, and the emulator reached the gate over
  a host relay rather than being cabled to `br-lan`, so for the phone run openNDS
  was not in the packet path (the module's gate call was real).
- The walled-garden/2121 handling was applied by hand in this rig, not by the
  module's own packaging. Whether the shipped module does this on a stock router
  is unverified and is the next thing to check.
- Debug-signed APK; automatic spending remains disabled.
