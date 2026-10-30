# Offline policy simulation

Build: `cargo build --features persistence --bin offline-policy-sim`.

Commands:
```
offline-policy-sim enroll DB ENROLLMENT_JSON
offline-policy-sim attempt DB EXPECTED_JSON SNAPSHOT_JSON VAULT SHA256 OPERATION DAY NOW [IGNORED_ADVERTISED_AMOUNT]
```

Enrollment JSON contains `caps` (unit-to-u64 map), `fee` (additional client-side upper bound), `snapshot_sha256` (lowercase digest), `valid_from`, and `valid_until` (exclusive). DB parent must already exist, private mode 0700. Enrollment exclusively creates the ledger and mode-0600 sibling `.policy.json`; interrupted enrollment fails closed and cannot reset an existing ledger. Neither caller attempts nor repeat enrollment can change caps or fee. No policy-update/reconciliation command exists.

Snapshot JSON contains `mint`, `unit`, and CDK `keysets`. An operator must explicitly pin trusted bytes: enrollment is NOT remote metadata authentication. Attempts require exact pinned bytes and NOW within the enrolled interval; unknown token/keyset/unit fails closed. DAY/NOW are trusted simulation inputs, not authenticated clocks. Same-UID/root mutation, filesystem rollback and power-loss durability are not established by this harness.

VAULT and SHA256 identify already stored bearer bytes. The adapter revalidates those exact bytes, reserves full face plus the enrolled fee, and commits send ownership before the fake callback. Advertised amount is ignored; it cannot reduce debit. Output is SIMULATION_CALLBACK plus a digest, never the bearer. Replay returns usage without another callback. There is no network transport, legacy buy dispatch, or payment fallback in this binary.

Tests: `cargo test --features persistence` includes six real subprocess tests and a production compile-negative raw API fence. `cargo test --features nonproduction-test-harness` deliberately enables the legacy raw callback tests, including races. Never enable that feature in production. The adapter does not depend on it.

This is not production payment authorization: mint provenance/fee authenticity, proof spendability and conditions, authenticated freshness, entitlement/network binding, no-consumption reconciliation, real router settlement and independently verified internet access remain unresolved. Watchers remain discovery-only.
