# Mint identity v1 (ledger schema 3)

The dormant offline validator and send-once store share one `url::Url`-based
identity function. It normalizes scheme/host case, removes default HTTP 80 / HTTPS
443 ports, and omits the final slash (including the root slash). HTTP and HTTPS
remain different. Nondefault ports, path case and internal path components are
preserved. Different hosts and path-based mints remain separate; no DNS, wallet,
redirect, reverse-proxy or server alias discovery is attempted.

The accepted subset is ASCII absolute HTTP(S) URLs with a host. Credentials,
queries, fragments, percent escapes, whitespace/control characters, backslashes,
dot path segments, internal repeated slashes, empty authorities/ports, trailing
DNS dots and noncanonical numeric IP spellings are rejected. This intentionally
narrow subset avoids URL parser repair and server-dependent interpretation.
CDK MintUrl already strips trailing slashes before this boundary; all such
trailing-slash spellings therefore share the same identity. IPv6 must already
use the URL parser's canonical host representation. The legacy send-plan label
restriction further limits supported persisted URLs; rejection is fail-closed.

Snapshot construction and decoded token comparison use this identity; overlap
and typed plan generation inherit it. `send_once` independently canonicalizes
mint strings for both operation replay and `(mint,Y)` ownership. Token bytes
remain unchanged in the protected vault.

## Existing ledger handling

Schema 2 persisted arbitrary mint strings and can contain independent alias
claims for the same proof. Schema 3 explicitly identifies the new policy.
**All schema 1/2 ledgers fail closed**, including empty ones: no automatic
migration, rewriting, merging, deletion, or re-enrollment. Existing reservations,
marks and claims must be retained for an explicit future reconciliation. Do not
change `user_version` manually or create a replacement ledger to bypass exposure.
Opening schema 3 also rejects noncanonical stored mint identities.

## Evidence and remaining gate

The exact review regression created two trusted snapshots for
`https://MINT.example:443/` and `https://mint.example`, identical proof Y/keyset,
and two **unmodified** typed plans with distinct operations. On base 63a3982 the
assertion failed with callbacks `2`, expected `1` (exit 101). With this fix only
one callback runs; overlap is detected. Matrix tests include restart, original
token spellings, distinct scheme/host/nondefault port/path, and old-schema refusal.
All fixtures and callbacks are synthetic; no funds or remote mint calls.

Mutable public plans remain a production blocker: callers can still substitute
proof IDs/amounts or closure payloads after validation. Structural validation
still proves neither signature authenticity nor spendability, and fee/metadata
provenance remain caller responsibilities. No CLI/JNI/watcher/payment integration
is enabled. Tmpfs tests are not persistent-disk power-loss evidence.
