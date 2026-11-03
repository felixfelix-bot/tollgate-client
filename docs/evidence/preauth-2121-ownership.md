# Is the pre-auth 2121 exemption the module's job, or the operator's?

Investigated 2026-11-03 by reading `tollgate-module-basic-go` @ `eb12d7f8`.
Motivated by the lab run where a pre-auth client could not reach the payment API
until 2121 was hand-added to openNDS's walled garden.

## Answer: the module already does it, for its declared NDS.

`packaging/Makefile:79`: `DEPENDS:=+nodogsplash +jq +ca-bundle`. nodogsplash is
a hard runtime dependency, and the module is the single writer of its pre-auth
allow list.

`packaging/files/etc/uci-defaults/99-tollgate-setup`,
`assert_nodogsplash_allow_entries()` (around line 1722) is that writer. It adds,
idempotently, and removes anything that is not on the contract:

- `allow tcp port 2121` — **the backend API; this is the exemption in question**
- `allow tcp port 2050` — NDS gateway
- `allow tcp port 2051` — portal SPA
- DNS `tcp/53` + `udp/53`
- `udp/123` — NTP

The header comment states the rule plainly: the list says what a
**pre-authentication** client may reach *on the router*, and `:2121` is
explicitly on it, while every admin surface (`:8090`, LuCI `:8080`/`:443`) is
deliberately absent and actively removed.

`30-backend-firewall.nft` then restricts 2121 at the fw4 layer by *ingress
interface* (`iifname != { br-lan, br-private, lo } drop`), i.e. it assumes NDS
has already let the flow reach the router and only stops it coming from WAN. The
two layers are designed together.

## So why did the lab need a hand-written rule?

**I installed openNDS, not nodogsplash.** openNDS is a different NDS with a
different config schema (`walledgarden_fqdn_list` / `walledgarden_port_list`) and
a different chain layout (`ip nds_filter ndsRTR` at hook priority -100, which
runs before fw4's `inet fw4`). Nothing in the module writes openNDS config — the
only mention of `opennds` anywhere in the tree is the filename
`tests/cloud-lab/fake-ndsctl.sh`.

So the lab finding is real but narrower than it first looked:

- **On a module-supported install (`nodogsplash`), the exemption is automatic.**
  No operator action needed.
- **On an openNDS install, the module's gate call and the module's own fw4
  fragment will still work, but a pre-auth client cannot fetch the advert until
  2121 is added to openNDS's walled garden.** That is an unsupported
  configuration, not a defect in the shipped path.

## Consequence for the acceptance verdict

The computer-side enforcement run in `enforcement-2026-11-03.md` used openNDS and
therefore needed two manual shims (the `ndsRTR` insert and the walled-garden
entry). That run still proves the *protocol* path end to end — advert, invoice,
settlement, grant, authenticated session, TLS-validated internet — but it proves
it on a stack the module does not ship.

**To claim the shipped deployment works, the same run must be repeated with
nodogsplash** (the supported NDS), where no 2121 shim should be necessary. That
is the correct next experiment, and it is also a sharper one: if nodogsplash
enforces correctly without hand-written rules, the remaining gap is only real
Lightning and real hardware.

## Also worth reporting upstream

The module's `Replaces:` reasoning and the runtime-dependency comment in
`packaging/Makefile` are explicitly about nodogsplash. If openNDS support is
intended for the apk era on 25.12 (openNDS is the maintained successor), the
pre-auth writer would need a second implementation and a fence between them, per
the repo's "one declaration per fact" doctrine. Nothing suggests it is intended
today — but a reader who installs openNDS will lose the payment entry point, and
that is worth a sentence in the README at minimum.
