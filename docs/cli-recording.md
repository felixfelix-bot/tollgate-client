# Actual CLI recording-only engine

Build `cargo build --locked --features persistence --bin tollgate-agent` on Linux.
Explicit syntax (recording flag must be first):
```
tollgate-agent --recording-only enroll --simulation-state NEW_DIRECTORY --simulation-policy expected.json
tollgate-agent --recording-only attempt --simulation-state NEW_DIRECTORY --simulation-policy expected.json --simulation-snapshot snapshot.json --simulation-digest SHA256 --simulation-operation STABLE_OPERATION
```
Enrollment/snapshot formats are in [offline-policy-adapter.md](offline-policy-adapter.md).
Use synthetic proofs only. Provision them with ProtectedTokenVault under NEW_DIRECTORY/vault;
there is deliberately no wallet import, funding, minting or automatic token selection command.
Enrollment creates a previously nonexistent private directory, dedicated ledger and
SIMULATION_ONLY marker. It never adopts any existing directory, wallet or ledger.
Attempts require this marker; paths to arbitrary wallet/vault/ledger are not accepted.
Interrupted enrollment is stranded, never reset. Persist and reuse operation and vault digest.

Dispatch precedes legacy options, environment, discovery and wallet construction. Only the
recording transport is passed to the existing enrolled adapter; no new money engine.
Unknown/duplicate/missing or legacy flags fail closed; legacy monetary environment variables
are ignored. Missing cdk/persistence/Linux support refuses rather than falling back.
Clock is SystemTime Unix seconds; budget day is the same sample divided by 86400 (UTC).
There is no NOW/DAY override. Trusted system time, not authenticated time.

Typed JSON reports mode simulated and enrolled, recorded_unresolved, unresolved or refused.
A recording is not payment or access. The durable JSONL records only operation/digest, not
bearer bytes. Full face plus fee remains reserved; restart never repeats the callback.
A record-write failure remains unresolved, not retryable. Successful replay exits 0 but says
unresolved, not success at payment. Runtime policy refusals exit 64; missing features exit 78.

`cargo test --features persistence --test cli_recording` exercises the actual binary across
processes. `--features nonproduction-test-harness` adds a test-only pause immediately inside
the callback (after committed send mark, before recording). Parent SIGKILL then reopen proves
zero records and retained usage. That pause option is rejected in normal builds. Tests use
/dev/shm: process-crash evidence, not power-loss persistence or device acceptance.

Remaining gates: independent review; authenticated mint/snapshot and clock, proof spendability,
network/entitlement binding, reconciliation, actual settlement and verified access. Existing
same-UID/root mutation and rollback exclusions remain. Duplicate cap-map keys and policy/ledger
identity binding are now rejected/bound; bounded ledger filesystem protection is documented in
[ledger-storage.md](ledger-storage.md). The marker is namespace separation,
not authentication against a malicious local user. Watchers remain discovery-only.
