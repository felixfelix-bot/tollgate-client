# First purchase from a real TollGate

> **Redaction note.** Device addresses, the router's LAN address and the client
> MAC in this transcript are placeholders (`02:00:00:00:00:01`, `<router-ip>`,
> `<gateway-ip>`). Do not "restore" them from a bench log — the topology lesson is
> the artifact, the identifiers are somebody's network.

Target: `10.99.99.1` — an OpenWrt VM (`Linux 6.6.86`, `br-lan`) running
`tollgate-module-basic-go`, reachable over a host bridge (`tg-poc-br`,
`10.99.99.2/24`). Mint: `https://testnut.cashu.exchange`, the one the router
advertises. **A test mint pays its own invoices**, so this moved no real money —
it is a real protocol exchange, not a real payment.

## The advert, as served (kind 10021, signed)

```
{"kind":10021,"pubkey":"0a6323…","tags":[
   ["metric","bytes"],
   ["step_size","22020096"],
   ["tips","1","2"],
   ["price_per_step","cashu","1","sat","https://testnut.cashu.exchange","1"]]}
```

## What worked

```
$ tollgate-agent --gate http://10.99.99.1:2121/ --wallet cdk --test-mint \
                 --token-margin 20 --seed-file … --wallet-db … --attempt-id 952
test-mint: 21 sat invoice not paid by us (the mint settles it): lnbc210n1qyqqqqq4808rcyf…
PROBE  offers=1 decision=Buy { steps: 1, cost: 1.0, unit: "sat" }
RESULT key=13971daa9f5d0109 state=granted cost=1
       outcome=Granted { allotment: Some(440401920), metric: Some("bytes"),
                         device_identifier: Some("mac") }

$ curl http://10.99.99.1:2121/session-state
{"status":1,"mac":"02:00:00:00:00:01","state":"active"}
```

440,401,920 bytes = 20 × 21 MiB. Access really opened: the session went from
`none` to `active`.

## What the router taught us on the way there

Three refusals, each correct and each a different lesson:

1. **`payment-error-invalid-token`** — a hand-written dummy token. The gate
   verifies with the mint before believing anything.
2. **`payment-error-below-swap-fee`** — a *real* 1 sat token. A note worth exactly
   the price is worth less than the price once the mint takes its swap fee, so
   the gate cannot absorb it. Hence `--token-margin`: mint the price plus a fee
   buffer. This is real economics, not a bug, and it belongs in the client's
   defaults discussion rather than in a retry loop.
3. **`client-not-registered`** — the pre-flight in `merchant.go:1543`: if the
   captive-portal (NDS) does not know the MAC, accepting the token would consume
   the customer's money with no session to open. It refuses **before** `Receive`,
   so the note is untouched. Our classification is `Retryable`, which is exactly
   right: the same 21 sat note could be re-submitted after the client appears to
   NDS.

   The fix was to do what a phone does on join: follow the portal redirect to
   `:2050/splash.html`. After that, the same purchase granted.

## Observations worth an upstream conversation

- The granted notice carries `["device-identifier","mac"]` — the literal string,
  not the MAC address the gate granted. Any client that trusts that tag to
  confirm "the gate opened for *my* device" would compare `"mac"` to its own MAC
  and conclude the answer was for someone else.
- The Lightning lane on this VM created real invoices and reached `state: PAID`
  against the test mint, but `access_granted` never became true and
  `/session-state` stayed `none` across two attempts (25 and 12 polls). The
  contract says that outcome should surface as `503 access-grant-failed`; it did
  not. Worth checking whether the grant hook is wired for the LN path in this
  build.
