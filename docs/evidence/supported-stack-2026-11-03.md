# Supported stack (nodogsplash): client buys and gets real internet — no shims

Run 2026-11-03 09:39 CET. Client `c3cd2f8`. This supersedes the openNDS run as
the acceptance-relevant result: **nodogsplash is the NDS the module declares and
configures** (`DEPENDS:=+nodogsplash`, `assert_nodogsplash_allow_entries()`), so
this is the shipped path.

## Rig

- OpenWrt 24.10.1 x86-64, QEMU/KVM, fresh overlay `router-nds.qcow2`, one TAP
  NIC cabled to bridge `br-tg` (**no slirp on the customer side**, so no NAT
  shortcut). Second slirp NIC is the router's WAN.
- Lab client: netns `tglab`, `veth-tg1` = `10.99.99.50` on the same L2 as the
  router's `br-lan` (`10.99.99.1`, renumbered off 192.168.1.0/24 because that
  collides with the host's real LAN and silently answered for the router).
- **nodogsplash 5.0.2** — the module's declared NDS. Its pre-auth allow list was
  populated exactly as the module's own `assert_nodogsplash_allow_entries()`
  writes it: `allow tcp port 2121`, `2050`, `2051`, DNS `tcp/53` + `udp/53`,
  `udp/123`. **No `nft insert`, no walled-garden override, no shims.**
- Module `eb12d7f8`, `CGO_ENABLED=0` amd64 at `/opt/tg/tg` under procd.
- Mint: host `cdk-mintd/0.18.3`, `fakewallet`, advertised as `10.0.2.2:3338`.

## Result

Pre-auth, with only the module's allow list in place:

```
$ curl -s http://10.99.99.1:2121/            # from 10.99.99.50
{"kind":10021,…,"tags":[["metric","bytes"],["step_size","22020096"],
 ["price_per_step","cashu","1","sat","http://10.0.2.2:3338/","1"]]}
$ curl -sI http://1.1.1.1/                    # external, still gated
HTTP/1.1 500 Internal Server Error
$ ndsctl clients
  IP: 10.99.99.50 MAC: 2e:c5:0f:63:d2:35
  Session Start: -
```

So the advert is reachable pre-auth **because the module put 2121 on the list**,
and the client is still preauthenticated (no session).

Purchase:

```
$ sudo ip netns exec tglab ./target/release/tollgate-agent --lightning \
    --gate http://10.99.99.1:2121/ --timeout-ms 8000 --max-polls 30 --poll-ms 2000
PAY THIS INVOICE (1 sat):
lnbc10n1p4wn27udqqpp5fat7mwylgsgnyvlp4yzfa8lckm64gl2j6ce0hl6zhh9rpusxda4q…
PROBE gate=http://10.99.99.1:2121/ offers=1 decision=Buy { steps: 1, cost: 1.0, unit: "sat" }
RESULT state=granted allotment=22020096 metric=bytes
```

Router, independently:

```
$ ndsctl status | grep -E 'Current clients|authentications since'
Client authentications since start: 1
Current clients: 1
$ ndsctl clients
  state=Authenticated
  active=1793698793
  token=02301144
```

Delivered internet, same netns, TLS certificate validation ON:

```
$ curl -sS --resolve one.one.one.one:443:1.1.1.1 https://one.one.one.one/cdn-cgi/trace
fl=928f56
h=one.one.one.one
ip=167.235.218.44
visit_scheme=https
colo=FRA

$ curl -sS --resolve api.github.com:443:140.82.121.6 https://api.github.com/zen
Accessible for all.
```

## What this corrects

The earlier openNDS run needed two hand-written rules, and I reported that as a
possible module gap. It is not. openNDS is a different NDS with a different
config schema, and the module writes none of its config — its only mention of
`opennds` in the tree is the filename `tests/cloud-lab/fake-ndsctl.sh`. On the
declared NDS the exemption is automatic.

Remaining unsupported-config note for upstream: an operator who installs openNDS
instead of nodogsplash loses the payment entry point (pre-auth clients cannot
reach :2121), which is worth one README sentence.

## Still not proven

- `fakewallet` mint — settles for free, no real sats, no real Lightning.
- Physical handset; the phone half was verified on the emulator, and that run
  reached the gate through a host relay rather than being cabled to `br-lan`, so
  nodogsplash was not in the phone's packet path.
- The APK is debug-signed.
- The token/ecash lane is not wired to the UI; Lightning is the only buy path.
