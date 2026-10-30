# Capped purchase orchestration

`capped_purchase` connects the reviewed pieces into one honest vertical step:
watch the session, pay at most once, watch the session again, and write down what
actually happened. It adds no new trust and removes none.

Requires `cdk` + `persistence` on Linux/Android. Lab-free: every test binds a
loopback `TcpListener`; no router, mint, VM or funds are involved.

## The three parts

| Piece | What it is |
| --- | --- |
| `SessionReader` | bounded, read-only `GET /session-state` on the same `BoundTarget` + `NetworkContext` + `Limits` as the POST |
| `ObservingTransport<T>` | `VerifiedBytesTransport` wrapper: forwards once, classifies the outcome, writes a durable per-operation record |
| `purchase_once` | baseline → send once through `PolicyAdapter::attempt` → bounded poll → one evidence file |

`SessionReader` is a **new module on purpose**. `bound_transport` is a reviewed,
single-attempt POST path; adding a GET to it would mean editing that path. The
reader copies the *style* (numeric target, explicit source and interface, no
redirect following, whole-request deadline, strict framing, typed outcome) and
shares nothing mutable with it.

## `SessionReader`

- The target path must be exactly `/session-state`; anything else is
  `WrongPath` **before a socket is opened**, so this can never be pointed at `/`.
- `SO_BINDTODEVICE` plus an explicit source address; a binding failure is an
  error, never an unbound fallback. Local and peer addresses are re-checked
  before a byte is written.
- Only `200` with a declared `Content-Length` is accepted. `3xx` is
  `Redirect{status}` (never followed, `Location` never contacted), any other
  status is `UnexpectedStatus`, `Transfer-Encoding` in any form is
  `UnsupportedFraming` (chunked is refused, not half-parsed), a missing
  `Content-Length` is `MissingFraming` (never read-to-EOF), and the body is
  bounded by `min(limits.max_response_bytes, MAX_SESSION_BODY)`.
- Body: a JSON object with a string `state`, mapped by **exact** match to
  `none`/`active`; everything else is `Unrecognized`. `allotment` must be a
  positive integer (number or string), `metric` a non-empty string. The typed
  result also carries `raw_digest`, the SHA-256 of the exact answer bytes, so the
  record can be tied to a `curl /session-state` taken at the same moment.

An `Unrecognized` session is **not** "not active". It blocks the purchase: see
below.

## `ObservingTransport` and the outcome mapping

`VerifiedBytesTransport` returns only a `DeliveryObservation`, which is all the
ledger needs. Orchestration also needs to know *why*, so this module adds
`ObservedTransport { send_observed -> SendOutcome }` (implemented for
`BoundVerifiedBytesTransport`) and `parse_send_outcome`.

| `SendOutcome` | `DeliveryObservation` |
| --- | --- |
| `Complete` + `200` + kind `1022` with `allotment` **and** `metric` | `GrantNotice` |
| `Complete` but non-2xx, not JSON, not kind 1022, missing/duplicate/zero tag, non-string tag | `RefusalOrMalformed` |
| `Ambiguous` | `Unknown` |
| `RefusedBeforeSend` | `Unknown` — see below |

### Why `RefusedBeforeSend` maps to `Unknown`

`BudgetStore::send_validated` commits the reservation, the proof claims and the
irreversible send mark in **one transaction before it calls the transport**.
By the time `ObservingTransport::send` runs, this process has already spent its
right to decide "nothing was sent, therefore nothing is owed".

So `RefusedBeforeSend` is not reported as `RefusalOrMalformed` — that would tell
the ledger the gate answered something. It maps to `Unknown`: the exposure is
already durable, and reporting it conservatively keeps that exposure. The
distinction is preserved where a human can act on it, as
`pre_send_refusal: true` in the observation record and the evidence file.

### Outstanding gap: manual reconciliation

Nothing in this module releases exposure. A `RefusedBeforeSend` (gate
unreachable, interface vanished, source bind failed) leaves the note owned and
the budget reserved until an operator reconciles against the mint (NUT-07) and
the ledger. Mitigations, in order:

1. **Preflight reachability check** — the baseline session read must *succeed*
   before any spend is attempted, so a gate that is down or misrouted is
   discovered before the send mark exists. This is what keeps the
   `RefusedBeforeSend` case rare rather than routine.
2. **One bounded POST** — no retry, no reconnect anywhere in the path.
3. **An honest record** — the evidence separates "provably wrote no byte" from
   "we do not know", which is the input a reconciler needs.

## `purchase_once`

Order is the contract:

1. **Ledger first.** If the operation already owns a send mark, the call is
   status-only: nothing is sent, and the persisted delivery is what gets
   classified. This happens *before* the baseline so a replay of an unresolved
   attempt is never mislabelled as "access already exists".
2. **Baseline** (also the preflight check). `active` → `SessionAlreadyActive`
   with **zero POSTs**. Read error or `Unrecognized` → `Unresolved` with **zero
   POSTs**; sending would commit the irreversible mark against a state that
   cannot be compared afterwards.
3. **Send once** via `PolicyAdapter::attempt` → `send_validated` →
   `ObservingTransport`. A concurrent owner that wins between 1 and 3 is caught
   after the fact (`transport.sends() == 0`) and treated as a replay.
4. **Observe** — bounded `PollPolicy` (attempts + interval, no unbounded wait,
   no "poll until active"). Polling is skipped only for a proven pre-send
   refusal, where no byte reached the gate.
5. **Publish** — one evidence file, written temp → fsync → rename → dir fsync
   into a `0700` directory as `0600`.

### Verdicts

| Verdict | Meaning | Reports access? |
| --- | --- | --- |
| `GrantAndSessionActive` | usable grant notice **and** an independently observed `active` session | **yes, only this** |
| `GrantWithoutSession` | grant notice, session never became active — money may be gone, access not obtained | no |
| `SessionAlreadyActive` | baseline was already `active`, nothing sent | no |
| `Refused` | the gate answered and it was not a usable grant, or the client refused to attempt | no |
| `Unresolved` | ambiguous delivery, pre-send refusal, unusable baseline, or unreadable session | no |

An `active` session observed under an **ambiguous** delivery is recorded
(`session_active_without_grant_notice`) but still yields `Unresolved`: access we
cannot attribute to a grant we saw is not access we confirmed.

## Evidence

`EVIDENCE_SCHEMA = "tollgate.capped-purchase.evidence.v1"`, exactly 13 keys,
all always present (asserted by the tests, because serde would otherwise accept a
missing `Option`):

```
schema, operation, offer_digest, payment_endpoint_digest, session_endpoint_digest,
network {interface, source}, baseline {...}|null, baseline_error,
send {attempted, replayed, delivery, notice, pre_send_refusal, outcome_detail,
      attempt_error},
post_session {...}|null, timestamps {started_unix, finished_unix, elapsed_ms},
verdict, notes[]
```

No bearer bytes, no token substring, no token-derived digest. Session/notice
`raw_digest` values are digests of **the gate's answers**, which is what makes a
later reconciliation possible without keeping those answers. `OBSERVATION_SCHEMA`
writes one `*.send.json` per forwarded body with the same discipline plus the
endpoint digest and network fingerprint.

Test `read_evidence` asserts the schema string, the exact key set, that no token
substring (first 24 bytes) appears, that no `cashu` appears at all, and that
every digest is 64 hex characters.

## Evidence: tests

`umask 077; TMPDIR=/dev/shm cargo test --features persistence` — 160 tests pass,
of which 12 are `tests/capped_purchase.rs` and 6 are in-module.

`tests/capped_purchase.rs` (loopback `TcpListener` only, assertions on what the
**server** observed):

| Test | Proves |
| --- | --- |
| `grant_plus_observed_active_session_is_the_only_access_verdict` | exactly one POST; grant + `none`→`active` yields the only access verdict; ≥2 GETs; evidence is strict JSON, token-free, one `*.send.json` |
| `grant_without_an_active_session_is_not_access_and_never_resends` | one POST, `GrantWithoutSession`, bounded polling, no access |
| `an_already_active_session_refuses_with_zero_posts` | zero POSTs, one GET, `SessionAlreadyActive`, `delivery: "not_sent"` |
| `an_ambiguous_answer_keeps_exposure_and_replay_is_status_only` | unframeable 200 → `Unknown`/`Unresolved`; session really opens and is still not called access; replay is status-only with one POST total |
| `a_pre_send_refusal_is_recorded_as_unknown_not_as_a_grant_or_refusal` | dead payment port → transport ran, `pre_send_refusal: true`, `Unknown`, zero POSTs, replay still sends nothing |
| `a_redirect_on_the_session_reader_stops_the_purchase_before_any_send` | `302` → `baseline_error: "redirect"`, zero POSTs, `Location` never contacted |
| `an_unrecognised_baseline_state_blocks_the_purchase` | `state: "provisioning"` → zero POSTs, `Unresolved` |
| `the_session_reader_is_bound_to_the_numeric_target_and_interface` | one `GET /session-state`, no POST, active parsed with allotment/metric/digest; wrong path refused before I/O |
| `a_non_2xx_or_unframed_session_answer_is_a_typed_error_not_a_guess` | 503 / no framing / chunked / oversized / duplicate length / truncated / trailing bytes / not JSON / no state all typed, zero POSTs |
| `complete_kind_1022_with_allotment_and_metric_is_a_grant_notice` | the grant mapping |
| `complete_but_not_a_usable_grant_is_a_refusal_or_malformed` | six malformed/non-2xx shapes |
| `ambiguous_and_pre_send_refusals_both_report_unknown` | the two `Unknown` mappings, with the pre-send flag |

RED evidence: every one of these was re-checked by reverting the specific
behaviour and confirming the test fails. Eleven mutations, each producing RED and
nothing compiling by accident:

| Mutation | Tests that fail |
| --- | --- |
| `RefusedBeforeSend` → `RefusalOrMalformed` | 3 (unit + integration) |
| grant alone reports access | 2 |
| no baseline-active refusal | 1 |
| no ledger-first replay check | 2 |
| grant without `metric` accepted | 1 |
| session reader accepts chunked | 1 |
| evidence leaks bearer bytes | 2 |
| evidence gains an extra key | 2 |
| evidence omits a null key | 2 |
| no polling after the send | 3 |
| baseline read error treated as "not active, proceed" | 1 |

`cargo fmt --check` and `git diff --check` pass. `--no-default-features
--features persistence` builds (the module is gated on `cdk`).

## Deferred (not stubbed — absent)

- **Exposure release / automatic reconciliation.** A `RefusedBeforeSend` or an
  ambiguous delivery stays owned until a human reconciles against the mint
  (NUT-07) and the ledger. `BudgetStore::settle`/`release` are not called from
  here; deciding "the note survived" is exactly the judgement this module refuses
  to guess.
- **Post-send route re-verification.** The session read goes to the same numeric
  endpoint and binding, but nothing re-proves that this request still traverses
  the intended path after the gate granted (proxy/NAT insertion between send and
  read is not detected).
- **Authenticated clock and trusted snapshot.** `now`, `day` and the snapshot
  bytes are still caller-supplied; `PolicyAdapter` re-hashes and re-checks them
  but does not authenticate their provenance.
- **Automatic retry after a proven-unspent reconciliation.** Deliberately absent:
  a second send is a new decision, not a resumption.
- **Multi-operation budgeting.** One operation, one send; nothing here schedules
  or serialises several operations.
- **Android execution.** Same gating as the rest of the ledger (`Linux/Android`
  arch flags), but only Linux has been exercised.
- **No CLI/JNI activation.** `purchase_once` has no caller in the shells yet.
