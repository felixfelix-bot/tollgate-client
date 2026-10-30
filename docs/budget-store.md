# Durable budget-store foundation (not a payment engine)

Enable `persistence` explicitly: `cargo test --no-default-features --features persistence`.
It adds rusqlite/bundled SQLite and requires a C compiler. Builds without this feature
have NO durable budget backend; no no-dependency persistence claim is made.
Android NDK persistence builds have not been verified. Watchers remain discovery-only.

`BudgetStore::create` is explicit new-ledger enrollment (create-new, Unix 0600,
file and parent-directory sync). `open` never creates a missing ledger or repairs
corruption. Existing databases require exact schema version 1, complete schema,
SQLite integrity and accounting consistency. Use a pre-created private directory
on a local filesystem with working SQLite locks and fsync; network filesystems,
external SQL writers, deletion/rollback of the DB and backup restoration are outside
this foundation's trust boundary. Failed creation leaves a blocking file, not a
silently usable empty budget. Do not auto-create after open fails.

Amounts are integer minor units restricted to 0..=i64::MAX. Face value must be
positive. Reserve face + additional fees with checked arithmetic: token 8 sat +
external routing cap 1 sat reserves 9, irrespective of advertised price. Fees
already included in face value are not added again. Units are exact case-sensitive
identifiers: sat and msat never share a balance or convert automatically. Callers
must validate canonical units, full token value, fee bounds and persisted policy.

Admission is today's settled debit + ALL unresolved reservations across days +
new reservation <= caller-supplied daily cap for the same unit. This low-level
store does not authorize the caller's choice of cap. BEGIN IMMEDIATE serializes
admission and terminal transitions across processes; unique IDs/terminal indexes
prevent replay debits. Synchronous FULL / DELETE journal mode is explicit.

Reservation ID doubles as its stable operation ID; settlement/release have globally
unique IDs in the same namespace. Callers must persist and reuse IDs; new IDs are
not proof deduplication. Identical retries are no-ops, changed payloads conflict.
A successful repeated reserve is NOT permission to send again. Terminal operations
are sticky. Final settlement books actual <= reserved on the supplied observation
UTC day and releases the rest; only use it when remaining exposure is resolved.
Release means proven zero debit; fees incurred require settlement, not release.
Partial or ambiguous outcomes must retain the original full reservation for now.

UTC day is supplied as an integer epoch-day by a trusted caller. A persisted
high-watermark rejects new transitions into earlier days; exact replay is allowed.
Forward-jump validation and operator clock recovery remain engine responsibilities.
Read-only usage queries may inspect historical days and include current exposure.

No CLI/JNI/wallet wiring, proof parsing/ownership, entitlement identity, external
operation lock, legacy migration, clock validation service, NUT-07 recovery,
funding, network-bound POST or access evidence is implemented here. Existing manual
payment paths remain legacy/unsafe. No funds, mint/router changes or deployment.
