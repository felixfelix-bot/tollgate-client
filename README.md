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

## Pending

`discovery` (kind:10021 parse, `:2121` probe, probe-IE hint) and `purchase`
(Cashu via `cashubtc/cdk`, NUT-07 reconciliation) land after the recon verdict —
the v1 client contract is being extracted from `OpenTollGate/tollgate-module-basic-go`.

## Test

```bash
cargo test          # 8 tests: math, ranking, reliability, caps, hysteresis, min_steps
```
