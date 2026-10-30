# tollgate-client

TollGate **v1** client core, shared by two shells:

- **Android** — Kotlin/Jetpack Compose + foreground service, Wi-Fi via
  `WifiNetworkSuggestion` (stock Android cannot silently auto-join; the OS
  mediates, the app buys the moment it is joined).
- **Debian** — `tollgate-agent` daemon + `tollgate-ctl` (CLI/TUI) + a
  NetworkManager dispatcher, which *can* join and switch fully automatically.

## What exists today

`scoring` + `policy`: the reliability / price / quality engine. Protocol-free.
Given an advertised offer (`price_per_step`, `step_size`, `metric`, `unit`,
`min_steps`) and that gateway's measured history, it answers **buy or not, from
whom, how many steps** — with hard caps first and hysteresis against flapping.

Deliberate rules:
- price is normalised from the advert's own fields to cost/minute (time) or
  cost/MB (bytes); **no exchange rate is ever invented** across units;
- success probability is Beta(1,1)-smoothed;
- quality blends RTT, throughput and loss; an unmeasured gateway scores 0.5;
- `max_cost_per_minute` and the daily budget are gates evaluated before value,
  and crossing either returns `Prompt`, never a silent purchase.

`discovery::parse_advertisement` parses the module's own kind:10021 tag grammar
(dependency-free: the shell decodes JSON). It is stricter than the merchant's
validator because this side pays: an advert with no metric or no usable
`step_size` is **rejected** (its price cannot become a rate), and an offer that
cannot be paid — wrong asset type, zero price, no unit, no mint — is **dropped**
rather than guessed at.

`purchase::classify_response` classifies the gate's answer — `200`+kind `1022` is a
grant, `400`+kind `21023` is a **definitive refusal**, and a `500`, an unparseable
`200`, or a timeout is **ambiguous** (the note may already be spent). Treating
that last class as a refusal is how a customer pays twice. `next_state` is a
forward-only state machine per attempt, and `intent_key` gives the journal a
stable handle that never stores the bearer token.

`agent::attempt_purchase` is the whole loop with the platform seams injected
(`Wallet`, `GateTransport`, `Journal`): caps are checked **before** a token is
minted, the attempt is journalled **before** the first network call, the answer is
classified, and an ambiguous answer is reconciled against the mint instead of
being retried.

## The Debian shell (runnable today)

```bash
cargo build --release --bin tollgate-agent
tollgate-agent --gate http://192.168.50.205:2121 \
               --token-file /run/secrets/note \
               --journal ~/.tollgate/journal.txt \
               --attempt-id "$(date +%s)" \
               --max-cost-per-minute 2 --daily-budget 100
```

It probes the advert, prices it, applies the caps, buys, classifies the answer
and journals the attempt — over a real socket, with **no dependencies**: the v1
gate is plain HTTP `:2121`, so `std::net::TcpStream` is enough. Exit `0` granted,
`2` not attempted / refused / already attempted, `1` error.

Two honest boundaries, both named in the code rather than papered over:
- the wallet is a `FileWallet` holding one pre-minted token; it **cannot answer
  NUT-07**, so an ambiguous answer surfaces as an error instead of a guess. The
  CDK wallet is the next deliverable;
- `gateway_id` is stamped with the gate URL because the advert carries no
  identity of its own. It works, but identity and address should be separate
  fields before multi-gateway ranking is meaningful.

## Pending

`:2121` probing + BSSID/probe-IE hints (needs a transport adapter per shell);
the Cashu wallet itself (`cashubtc/cdk`) behind a `Wallet` trait, plus NUT-07
reconciliation for ambiguous attempts — after the recon verdict from
`OpenTollGate/tollgate-module-basic-go`.

## Test

```bash
cargo test          # 8 tests: math, ranking, reliability, caps, hysteresis, min_steps
```
