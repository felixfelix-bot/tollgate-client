//! Loopback-only tests for the capped single-purchase orchestration.
//!
//! Every test binds a local `TcpListener` on 127.0.0.1, scripts the gate's
//! answers, and asserts on what the *server* observed: how many POST bodies it
//! received, and which session state it reported. A verdict that says "access"
//! without a second observed GET is a failure here.
//!
//! No router, no mint, no VM, no funds: the token is a synthetic Cashu note
//! signed by a throwaway key and the only payload that leaves the process is the
//! one this test's listener reads.

#![cfg(all(feature = "cdk", feature = "persistence", target_os = "linux"))]

use cdk::{
    dhke::{hash_to_curve, sign_message},
    mint_url::MintUrl,
    nuts::{CurrencyUnit, Id, KeySet, Keys, Proof, SecretKey, Token},
    secp256k1::hashes::{sha256, Hash},
    secret::Secret,
    Amount,
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use tollgate_client_core::{
    bound_transport::{
        AmbiguousReason, BoundTarget, Limits, NetworkContext, RefusalReason, SendOutcome,
    },
    capped_purchase::{
        delivery_tag, purchase_once, EvidenceSink, ObservedTransport, ObservingTransport,
        PollPolicy, PurchaseRequest, SessionPhase, Verdict,
    },
    offline_policy::{Enrollment, PolicyAdapter},
    send_state::DeliveryObservation,
    token_vault::ProtectedTokenVault,
    validated_send::VerifiedBytesTransport,
};

const GRANT: &str = r#"{"kind":1022,"tags":[["allotment","60000"],["metric","milliseconds"],["device-identifier","aa:bb:cc:dd:ee:ff"]]}"#;
const SESSION_NONE: &str = r#"{"status":1,"mac":"aa:bb:cc:dd:ee:ff","state":"none"}"#;
const SESSION_ACTIVE: &str = r#"{"status":1,"mac":"aa:bb:cc:dd:ee:ff","state":"active","allotment":60000,"metric":"milliseconds"}"#;

// ---------------------------------------------------------------- fixture ----

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    policy: Enrollment,
    keyset: KeySet,
    sk: SecretKey,
}

impl Fixture {
    fn new(cap: u64) -> Self {
        let root = PathBuf::from("/dev/shm").join(format!(
            "capped-purchase-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // The vault/ledger hardening refuses a group-writable home; the shared
        // memory filesystem is root-owned sticky and this directory is ours.
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        ProtectedTokenVault::open(&root).unwrap();
        let sk = SecretKey::from_str(&"01".repeat(32)).unwrap();
        let keys = Keys::new(
            [1u64, 2, 8]
                .into_iter()
                .map(|n| (Amount::from(n), sk.public_key()))
                .collect::<BTreeMap<_, _>>(),
        );
        let keyset = KeySet {
            id: Id::v1_from_keys(&keys),
            unit: CurrencyUnit::Sat,
            active: Some(false),
            keys,
            input_fee_ppk: 0,
            final_expiry: None,
        };
        let snapshot = serde_json::to_vec(&serde_json::json!({
            "mint": "https://mint.example",
            "unit": "sat",
            "keysets": [keyset.clone()],
        }))
        .unwrap();
        std::fs::write(root.join("snapshot.json"), &snapshot).unwrap();
        let policy = Enrollment {
            caps: [("sat".into(), cap)].into(),
            fee: 1,
            snapshot_sha256: sha256::Hash::hash(&snapshot).to_string(),
            valid_from: 10,
            valid_until: 20,
        };
        Self {
            root,
            policy,
            keyset,
            sk,
        }
    }

    /// Store one synthetic note in the vault and return (token text, digest).
    fn token(&self, secret: &str, amount: u64) -> (String, String) {
        let secret = Secret::from_str(secret).unwrap();
        let c = sign_message(&self.sk, &hash_to_curve(secret.as_bytes()).unwrap()).unwrap();
        let proof = Proof::new(amount.into(), self.keyset.id, secret, c);
        let text = Token::new(
            MintUrl::from_str("https://mint.example").unwrap(),
            vec![proof],
            None,
            CurrencyUnit::Sat,
        )
        .to_string();
        let vault = ProtectedTokenVault::open(self.root.join("vault")).unwrap();
        let reference = vault.store(text.as_bytes()).unwrap();
        let digest = reference
            .digest()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (text, digest)
    }

    fn snapshot(&self) -> Vec<u8> {
        std::fs::read(self.root.join("snapshot.json")).unwrap()
    }

    fn enroll(&self) -> PolicyAdapter {
        PolicyAdapter::enroll(&self.root.join("ledger"), &self.policy).unwrap()
    }

    fn context() -> NetworkContext {
        NetworkContext::new("lo", IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap()
    }

    fn limits() -> Limits {
        Limits {
            connect: Duration::from_millis(500),
            read: Duration::from_millis(500),
            write: Duration::from_millis(500),
            total: Duration::from_millis(1500),
            max_response_bytes: 4096,
            max_header_bytes: 2048,
        }
    }

    fn request(
        &self,
        pay: SocketAddr,
        session: SocketAddr,
        digest: &str,
        op: &str,
    ) -> PurchaseRequest<'_> {
        PurchaseRequest {
            enrollment: &self.policy,
            snapshot_bytes: self.snapshot(),
            ledger_path: self.root.join("ledger"),
            vault_path: self.root.join("vault"),
            reference: digest.to_string(),
            operation: op.to_string(),
            day: 1,
            now: 15,
            offer_digest: Some("00".repeat(32)),
            pay_target: BoundTarget::new_http(pay, "/").unwrap(),
            session_target: BoundTarget::new_http(session, "/session-state").unwrap(),
            context: Self::context(),
            limits: Self::limits(),
            poll: PollPolicy::new(3, Duration::from_millis(5)),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------- harness ----

struct Report {
    connections: usize,
    posts: Vec<Vec<u8>>,
    gets: Vec<String>,
}

struct Harness {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    rx: mpsc::Receiver<Report>,
    join: Option<thread::JoinHandle<()>>,
}

type Handler = Arc<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end: Option<usize> = None;
    let mut content_length = 0usize;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(i) = find(&buf, b"\r\n\r\n") {
                        header_end = Some(i + 4);
                        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                        for line in head.split("\r\n") {
                            if let Some(v) = line.strip_prefix("content-length:") {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if buf.len() >= end + content_length {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    buf
}

fn response(status: u16, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

impl Harness {
    fn new<F>(handler: F) -> Self
    where
        F: Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    {
        Self::on(("127.0.0.1", 0), handler)
    }

    fn on<F, A>(bind: A, handler: F) -> Self
    where
        F: Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
        A: std::net::ToSocketAddrs,
    {
        let listener = TcpListener::bind(bind).expect("bind loopback listener");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_in = stop.clone();
        let (tx, rx) = mpsc::channel();
        let handler: Handler = Arc::new(handler);
        let join = thread::spawn(move || {
            let mut report = Report {
                connections: 0,
                posts: Vec::new(),
                gets: Vec::new(),
            };
            while !stop_in.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        report.connections += 1;
                        // Exactly one read per connection: the handler sees the
                        // bytes the server actually received, and there is no
                        // second, phantom request pulling an EOF.
                        let request = read_request(&mut stream);
                        if request.starts_with(b"POST ") {
                            report.posts.push(request.clone());
                        } else {
                            report
                                .gets
                                .push(String::from_utf8_lossy(&request).to_string());
                        }
                        if let Some(raw) = handler(&request) {
                            let _ = stream.write_all(&raw);
                            let _ = stream.flush();
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(report);
        });
        Self {
            addr,
            stop,
            rx,
            join: Some(join),
        }
    }

    fn finish(mut self) -> Report {
        thread::sleep(Duration::from_millis(120));
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
        self.rx.recv().expect("harness report")
    }
}

/// A gate that answers one POST with a hard-coded grant and reports the current
/// session state on every GET. `active` flips only when the gate takes a POST, so
/// "the session became active" is caused by the send, not by the script.
fn gate(
    grant_body: &'static str,
    active: Arc<AtomicBool>,
    activate_on_post: bool,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request: &[u8]| {
        if request.starts_with(b"POST ") {
            if activate_on_post {
                active.store(true, Ordering::SeqCst);
            }
            Some(response(200, grant_body))
        } else {
            Some(response(
                200,
                if active.load(Ordering::SeqCst) {
                    SESSION_ACTIVE
                } else {
                    SESSION_NONE
                },
            ))
        }
    }
}

/// A gate whose POST answer declares more bytes than it sends: the client must
/// not read that as a grant, and must not resend. The session *does* open, so a
/// client that confused "ambiguous" with "granted" would report access here.
fn broken_gate(
    active: Arc<AtomicBool>,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request: &[u8]| {
        if request.starts_with(b"POST ") {
            active.store(true, Ordering::SeqCst);
            Some(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec())
        } else {
            Some(response(
                200,
                if active.load(Ordering::SeqCst) {
                    SESSION_ACTIVE
                } else {
                    SESSION_NONE
                },
            ))
        }
    }
}

/// Strict mirror of the evidence contract used to prove the file has exactly the
/// documented keys (unknown or missing fields fail).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictEvidence {
    schema: String,
    operation: String,
    offer_digest: Option<String>,
    payment_endpoint_digest: String,
    session_endpoint_digest: String,
    network: StrictNetwork,
    baseline: Option<StrictSession>,
    baseline_error: Option<String>,
    send: StrictSend,
    post_session: Option<StrictSession>,
    timestamps: StrictTimestamps,
    verdict: String,
    notes: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictNetwork {
    interface: String,
    source: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictSession {
    phase: String,
    allotment: Option<u64>,
    metric: Option<String>,
    raw_digest: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictSend {
    attempted: bool,
    replayed: bool,
    delivery: String,
    notice: Option<StrictNotice>,
    pre_send_refusal: bool,
    outcome_detail: Option<String>,
    attempt_error: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictNotice {
    allotment: u64,
    metric: String,
    raw_digest: String,
}

#[derive(serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct StrictTimestamps {
    started_unix: u64,
    finished_unix: u64,
    elapsed_ms: u64,
}

/// Read the evidence file back: strict JSON, exact key set, no bearer substring.
fn read_evidence(path: &std::path::Path, token: &str) -> StrictEvidence {
    let raw = std::fs::read_to_string(path).expect("evidence file readable");
    // `deny_unknown_fields` rejects extra keys; serde would still accept a
    // *missing* `Option` field, so presence is asserted explicitly. The contract
    // says every one of these keys is always written.
    let object: serde_json::Value = serde_json::from_str(&raw).expect("evidence is JSON");
    let object = object.as_object().expect("evidence is a JSON object");
    for key in [
        "schema",
        "operation",
        "offer_digest",
        "payment_endpoint_digest",
        "session_endpoint_digest",
        "network",
        "baseline",
        "baseline_error",
        "send",
        "post_session",
        "timestamps",
        "verdict",
        "notes",
    ] {
        assert!(
            object.contains_key(key),
            "evidence is missing `{key}`: {raw}"
        );
    }
    assert_eq!(object.len(), 13, "no undocumented keys: {raw}");
    for section in ["network", "send", "timestamps"] {
        assert!(object[section].is_object(), "{section} is an object");
    }
    let parsed: StrictEvidence = serde_json::from_str(&raw).expect("strict evidence JSON");
    assert_eq!(parsed.schema, "tollgate.capped-purchase.evidence.v1");
    let marker = &token[..24];
    assert!(
        !raw.contains(marker),
        "evidence must never contain the bearer token"
    );
    assert!(!raw.to_lowercase().contains("cashu"), "{raw}");
    // Every digest is a hex SHA-256 of something, never the something itself.
    for digest in [
        &parsed.payment_endpoint_digest,
        &parsed.session_endpoint_digest,
    ] {
        assert_eq!(digest.len(), 64, "{digest}");
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()), "{digest}");
    }
    for session in [&parsed.baseline, &parsed.post_session]
        .into_iter()
        .flatten()
    {
        assert_eq!(session.raw_digest.len(), 64, "{:?}", session.phase);
    }
    if let Some(notice) = &parsed.send.notice {
        assert_eq!(notice.raw_digest.len(), 64);
        assert!(notice.allotment > 0);
        assert!(!notice.metric.is_empty());
    }
    assert!(
        parsed.timestamps.started_unix > 0
            && parsed.timestamps.finished_unix >= parsed.timestamps.started_unix,
        "{:?}",
        parsed.timestamps
    );
    assert!(parsed.timestamps.elapsed_ms < 600_000, "bounded run");
    assert!(!parsed.notes.is_empty() || parsed.verdict == "grant_and_session_active");
    parsed
}

// ------------------------------------------------------------------ tests ----

#[test]
fn grant_plus_observed_active_session_is_the_only_access_verdict() {
    let fixture = Fixture::new(10);
    let (token, digest) = fixture.token("grant-path", 2);
    let mut adapter = fixture.enroll();
    let active = Arc::new(AtomicBool::new(false));
    let harness = Harness::new(gate(GRANT, active.clone(), true));
    let sink = EvidenceSink::open(fixture.root.join("evidence")).unwrap();

    let request = fixture.request(harness.addr, harness.addr, &digest, "capped-grant");
    let published = purchase_once(&mut adapter, &request, &sink).expect("purchase");
    let report = harness.finish();

    assert_eq!(report.posts.len(), 1, "exactly one POST server-side");
    assert_eq!(
        published.evidence.verdict,
        Verdict::GrantAndSessionActive,
        "{:?}",
        published.evidence
    );
    assert!(published.evidence.verdict.reports_access());
    assert_eq!(
        published.evidence.post_session.as_ref().unwrap().phase,
        "active"
    );
    assert_eq!(published.evidence.baseline.as_ref().unwrap().phase, "none");
    assert!(
        report.gets.len() >= 2,
        "a grant verdict needs its own session read, got {}",
        report.gets.len()
    );
    assert!(report.gets.iter().all(|g| g.contains("GET /session-state")));

    let strict = read_evidence(&published.path, &token);
    assert_eq!(strict.verdict, "grant_and_session_active");
    assert_eq!(strict.operation, "capped-grant");
    assert_eq!(strict.send.delivery, "grant_notice");
    assert!(!strict.send.replayed);
    assert!(strict.send.attempted);
    assert_eq!(strict.send.notice.as_ref().unwrap().allotment, 60_000);
    assert_eq!(strict.send.notice.as_ref().unwrap().metric, "milliseconds");
    assert_eq!(strict.network.interface, "lo");
    assert_eq!(strict.network.source, "127.0.0.1");
    assert!(
        strict.offer_digest.is_some(),
        "the answered offer is recorded"
    );
    assert!(strict.baseline_error.is_none());
    let post = strict.post_session.as_ref().unwrap();
    assert_eq!(post.phase, "active");
    assert_eq!(post.allotment, Some(60_000), "the gate's own figure");
    assert_eq!(post.metric.as_deref(), Some("milliseconds"));
    assert!(strict.baseline.as_ref().unwrap().allotment.is_none());
    assert!(!strict.send.pre_send_refusal);
    assert!(
        strict.send.outcome_detail.is_none(),
        "a clean grant has no caveat"
    );
    assert!(strict.send.attempt_error.is_none());

    // The per-operation transport observation is durable and token-free too.
    let mut observations: Vec<PathBuf> = std::fs::read_dir(fixture.root.join("evidence"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".send.json"))
        .collect();
    observations.sort();
    assert_eq!(observations.len(), 1, "one durable observation per send");
    let record = std::fs::read_to_string(&observations[0]).unwrap();
    assert!(!record.contains(&token[..24]));
    let record: serde_json::Value = serde_json::from_str(&record).unwrap();
    assert_eq!(record["delivery"], "grant_notice");
    assert_eq!(record["operation"], "capped-grant");
}

#[test]
fn grant_without_an_active_session_is_not_access_and_never_resends() {
    let fixture = Fixture::new(10);
    let (_token, digest) = fixture.token("grant-no-session", 2);
    let mut adapter = fixture.enroll();
    // The gate answers a grant notice but never opens a session.
    let active = Arc::new(AtomicBool::new(false));
    let harness = Harness::new(gate(GRANT, active.clone(), false));
    let sink = EvidenceSink::open(fixture.root.join("evidence")).unwrap();

    let request = fixture.request(harness.addr, harness.addr, &digest, "capped-no-session");
    let published = purchase_once(&mut adapter, &request, &sink).expect("purchase");
    let report = harness.finish();

    assert_eq!(report.posts.len(), 1, "a grant is never resent");
    assert_eq!(published.evidence.verdict, Verdict::GrantWithoutSession);
    assert!(
        !published.evidence.verdict.reports_access(),
        "a grant notice alone is never access"
    );
    assert_eq!(
        published.evidence.post_session.as_ref().unwrap().phase,
        "none"
    );
    // baseline + bounded poll (3 attempts), never unbounded.
    assert!(
        (2..=4).contains(&report.gets.len()),
        "bounded polling, got {} GETs",
        report.gets.len()
    );
}

#[test]
fn an_already_active_session_refuses_with_zero_posts() {
    let fixture = Fixture::new(10);
    let (_token, digest) = fixture.token("already-active", 2);
    let mut adapter = fixture.enroll();
    let active = Arc::new(AtomicBool::new(true));
    let harness = Harness::new(gate(GRANT, active, true));
    let sink = EvidenceSink::open(fixture.root.join("evidence")).unwrap();

    let request = fixture.request(harness.addr, harness.addr, &digest, "capped-active");
    let published = purchase_once(&mut adapter, &request, &sink).expect("purchase");
    let report = harness.finish();

    assert_eq!(report.posts.len(), 0, "no spend when access already exists");
    assert_eq!(published.evidence.verdict, Verdict::SessionAlreadyActive);
    assert!(!published.evidence.verdict.reports_access());
    assert_eq!(report.gets.len(), 1, "one baseline read, then stop");
    assert!(!published.evidence.send.attempted);
    assert_eq!(published.evidence.send.delivery, "not_sent");
}

#[test]
fn an_ambiguous_answer_keeps_exposure_and_replay_is_status_only() {
    let fixture = Fixture::new(10);
    let (token, digest) = fixture.token("ambiguous", 2);
    let mut adapter = fixture.enroll();
    let active = Arc::new(AtomicBool::new(false));
    let harness = Harness::new(broken_gate(active));
    let sink = EvidenceSink::open(fixture.root.join("evidence")).unwrap();

    let request = fixture.request(harness.addr, harness.addr, &digest, "capped-ambiguous");
    let first = purchase_once(&mut adapter, &request, &sink).expect("purchase");
    assert_eq!(first.evidence.verdict, Verdict::Unresolved);
    assert_eq!(first.evidence.send.delivery, "unknown");
    assert!(first.evidence.send.attempted);
    assert!(!first.evidence.send.pre_send_refusal);
    read_evidence(&first.path, &token);

    // Replaying the same operation must not reintroduce a POST: the ledger mark
    // is already irreversible, so the second call is status-only.
    let second = purchase_once(&mut adapter, &request, &sink).expect("replay");
    let report = harness.finish();

    assert_eq!(report.posts.len(), 1, "the replay never sends again");
    assert_eq!(second.evidence.verdict, Verdict::Unresolved);
    assert!(second.evidence.send.replayed, "{:?}", second.evidence.send);
    assert!(!second.evidence.send.attempted);
    assert_eq!(second.evidence.send.delivery, "unknown");
    assert_ne!(first.path, second.path, "each call keeps its own evidence");

    // Exposure is retained, not released: no settlement path ran, so the note is
    // still owned by the ledger and a human has to reconcile it.
    assert!(first
        .evidence
        .notes
        .iter()
        .any(|n| n == "delivery_unknown_exposure_retained"));
    assert!(!first.evidence.verdict.reports_access());
    // The session *did* go active in this script, and the client still refuses to
    // call that access: an ambiguous delivery is not a grant notice.
    assert_eq!(
        first.evidence.post_session.as_ref().unwrap().phase,
        "active"
    );
    assert!(first
        .evidence
        .notes
        .iter()
        .any(|n| n == "session_active_without_grant_notice"));
}

#[test]
fn a_pre_send_refusal_is_recorded_as_unknown_not_as_a_grant_or_refusal() {
    let fixture = Fixture::new(10);
    let (_token, digest) = fixture.token("pre-send-refusal", 2);
    let mut adapter = fixture.enroll();
    let active = Arc::new(AtomicBool::new(false));
    let harness = Harness::new(gate(GRANT, active, true));

    // A port that nothing listens on: connect is refused before any byte leaves.
    let dead = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);

    let sink = EvidenceSink::open(fixture.root.join("evidence")).unwrap();
    let session_addr = harness.addr;
    let request = fixture.request(dead_addr, session_addr, &digest, "capped-refused");
    let published = purchase_once(&mut adapter, &request, &sink).expect("purchase");
    let report = harness.finish();

    assert_eq!(report.posts.len(), 0, "nothing was sent to the gate");
    assert!(published.evidence.send.attempted, "the transport did run");
    assert!(published.evidence.send.pre_send_refusal);
    assert_eq!(published.evidence.send.delivery, "unknown");
    assert_eq!(published.evidence.verdict, Verdict::Unresolved);
    assert_eq!(published.evidence.baseline.as_ref().unwrap().phase, "none");

    // Exposure is retained: the send mark was committed before the refusal, so a
    // second call cannot send either.
    let replay = purchase_once(&mut adapter, &request, &sink).expect("replay");
    assert!(replay.evidence.send.replayed);
    assert_eq!(replay.evidence.send.delivery, "unknown");
    assert_eq!(replay.evidence.verdict, Verdict::Unresolved);
    assert!(
        report.posts.is_empty(),
        "the replay sent nothing to the payment port"
    );
}

// ------------------------------------------------------- observation mapper ----

struct Fake {
    outcome: Option<SendOutcome>,
    calls: usize,
}

impl VerifiedBytesTransport for Fake {
    fn send(&mut self, _body: &[u8]) -> DeliveryObservation {
        self.calls += 1;
        DeliveryObservation::Unknown
    }
}

impl ObservedTransport for Fake {
    fn send_observed(&mut self, _body: &[u8]) -> SendOutcome {
        self.calls += 1;
        self.outcome.take().expect("one scripted outcome")
    }
}

fn classify(outcome: SendOutcome) -> (DeliveryObservation, Option<u64>, Option<String>) {
    let mut transport = ObservingTransport::new(Fake {
        outcome: Some(outcome),
        calls: 0,
    });
    // Forward through the public trait, exactly as `send_validated` does.
    let delivery = VerifiedBytesTransport::send(&mut transport, b"token-bytes");
    let last = transport.last().expect("observation recorded");
    assert_eq!(transport.sends(), 1, "one forward, no retry");
    assert_eq!(
        last.delivery,
        delivery_tag(delivery),
        "trait view agrees with the durable record"
    );
    (
        delivery,
        last.notice.as_ref().map(|n| n.allotment),
        last.notice.as_ref().map(|n| n.metric.clone()),
    )
}

#[test]
fn complete_kind_1022_with_allotment_and_metric_is_a_grant_notice() {
    let (delivery, allotment, metric) = classify(SendOutcome::Complete {
        status: 200,
        body: GRANT.as_bytes().to_vec(),
    });
    assert_eq!(delivery, DeliveryObservation::GrantNotice);
    assert_eq!(allotment, Some(60_000));
    assert_eq!(metric.as_deref(), Some("milliseconds"));
}

#[test]
fn complete_but_not_a_usable_grant_is_a_refusal_or_malformed() {
    let cases: Vec<(&str, SendOutcome)> = vec![
        (
            "missing allotment",
            SendOutcome::Complete {
                status: 200,
                body: br#"{"kind":1022,"tags":[["metric","milliseconds"]]}"#.to_vec(),
            },
        ),
        (
            "zero allotment",
            SendOutcome::Complete {
                status: 200,
                body: br#"{"kind":1022,"tags":[["allotment","0"],["metric","bytes"]]}"#.to_vec(),
            },
        ),
        (
            "not a notice",
            SendOutcome::Complete {
                status: 200,
                body: br#"{"kind":21023,"tags":[["code","payment-error-token-spent"]]}"#.to_vec(),
            },
        ),
        (
            "not JSON",
            SendOutcome::Complete {
                status: 200,
                body: b"<html>captive portal</html>".to_vec(),
            },
        ),
        (
            "empty body",
            SendOutcome::Complete {
                status: 200,
                body: Vec::new(),
            },
        ),
        (
            "non-2xx complete",
            SendOutcome::Complete {
                status: 400,
                body: GRANT.as_bytes().to_vec(),
            },
        ),
    ];
    for (name, outcome) in cases {
        let (delivery, notice, _) = classify(outcome);
        assert_eq!(delivery, DeliveryObservation::RefusalOrMalformed, "{name}");
        assert!(notice.is_none(), "{name}");
    }
}

#[test]
fn ambiguous_and_pre_send_refusals_both_report_unknown() {
    let (delivery, notice, _) = classify(SendOutcome::Ambiguous {
        reason: AmbiguousReason::Truncated,
    });
    assert_eq!(delivery, DeliveryObservation::Unknown);
    assert!(notice.is_none());

    let (delivery, _, _) = classify(SendOutcome::Ambiguous {
        reason: AmbiguousReason::UnexpectedStatus(500),
    });
    assert_eq!(delivery, DeliveryObservation::Unknown);

    let mut transport = ObservingTransport::new(Fake {
        outcome: Some(SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::ConnectFailed("refused".into()),
        }),
        calls: 0,
    });
    let delivery = VerifiedBytesTransport::send(&mut transport, b"token-bytes");
    assert_eq!(
        delivery,
        DeliveryObservation::Unknown,
        "the irreversible send mark is already committed before transport runs"
    );
    let last = transport.last().unwrap();
    assert!(last.pre_send_refusal);
    assert!(last.detail.is_some(), "the reason is kept for the record");
}

/// A server that answers every connection with exactly these bytes.
fn constant(raw: &'static [u8]) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |_request: &[u8]| Some(raw.to_vec())
}

fn reader_at(addr: SocketAddr) -> tollgate_client_core::capped_purchase::SessionReader {
    tollgate_client_core::capped_purchase::SessionReader::new(
        BoundTarget::new_http(addr, "/session-state").unwrap(),
        Fixture::context(),
        Fixture::limits(),
    )
}

#[test]
fn a_redirect_on_the_session_reader_stops_the_purchase_before_any_send() {
    let fixture = Fixture::new(10);
    let sink = EvidenceSink::open(fixture.root.join("evidence")).unwrap();
    let (_token, digest) = fixture.token("reader", 2);
    let mut adapter = fixture.enroll();
    let redirect = Harness::new(constant(
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\n\r\n",
    ));
    let request = fixture.request(redirect.addr, redirect.addr, &digest, "capped-redirect");
    let published = purchase_once(&mut adapter, &request, &sink).expect("purchase");
    let report = redirect.finish();

    assert_eq!(
        report.posts.len(),
        0,
        "no POST without a trustworthy baseline"
    );
    assert_eq!(published.evidence.verdict, Verdict::Unresolved);
    assert_eq!(
        published.evidence.baseline_error.as_deref(),
        Some("redirect"),
        "a 3xx is never followed and never read as a session"
    );
    assert!(published.evidence.baseline.is_none());
    assert!(published
        .evidence
        .notes
        .iter()
        .any(|n| n == "no_trustworthy_baseline_no_send"));
    assert_eq!(report.connections, 1, "the Location is never contacted");
}

#[test]
fn an_unrecognised_baseline_state_blocks_the_purchase() {
    let fixture = Fixture::new(10);
    let (_token, digest) = fixture.token("weird-state", 2);
    let mut adapter = fixture.enroll();
    let harness = Harness::new(|request: &[u8]| {
        if request.starts_with(b"POST ") {
            Some(response(200, GRANT))
        } else {
            Some(response(200, r#"{"status":1,"state":"provisioning"}"#))
        }
    });
    let sink = EvidenceSink::open(fixture.root.join("evidence")).unwrap();
    let request = fixture.request(harness.addr, harness.addr, &digest, "capped-weird");
    let published = purchase_once(&mut adapter, &request, &sink).expect("purchase");
    let report = harness.finish();

    assert_eq!(
        report.posts.len(),
        0,
        "an unknown phase is not 'not active'"
    );
    assert_eq!(published.evidence.verdict, Verdict::Unresolved);
    assert_eq!(
        published.evidence.baseline.as_ref().unwrap().phase,
        "unrecognized"
    );
    assert!(published
        .evidence
        .notes
        .iter()
        .any(|n| n == "baseline_state_unrecognized"));
    assert_eq!(
        SessionPhase::from_wire("provisioning").as_str(),
        "unrecognized"
    );
}

#[test]
fn the_session_reader_is_bound_to_the_numeric_target_and_interface() {
    let active = Arc::new(AtomicBool::new(true));
    let harness = Harness::new(gate(GRANT, active, true));
    let addr = harness.addr;

    let state = reader_at(addr).read().expect("bounded GET succeeds");
    assert_eq!(state.state, SessionPhase::Active);
    assert!(state.is_active());
    assert_eq!(state.allotment, Some(60_000));
    assert_eq!(state.metric.as_deref(), Some("milliseconds"));
    assert_eq!(state.raw_digest.len(), 64);
    let report = harness.finish();
    assert_eq!(report.posts.len(), 0, "the reader never sends a body");
    assert_eq!(report.gets.len(), 1);
    assert!(report.gets[0].starts_with("GET /session-state HTTP/1.1"));

    // A path other than /session-state is refused structurally, before any I/O.
    use tollgate_client_core::capped_purchase::{SessionReadError, SessionReader};
    let wrong = SessionReader::new(
        BoundTarget::new_http(addr, "/").unwrap(),
        Fixture::context(),
        Fixture::limits(),
    );
    assert_eq!(wrong.read().unwrap_err(), SessionReadError::WrongPath);
}

#[test]
fn a_non_2xx_or_unframed_session_answer_is_a_typed_error_not_a_guess() {
    use tollgate_client_core::capped_purchase::SessionReadError;

    let cases: Vec<(&str, &'static [u8], SessionReadError)> = vec![
        (
            "503",
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
            SessionReadError::UnexpectedStatus(503),
        ),
        (
            "no framing",
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"state\":\"active\"}",
            SessionReadError::MissingFraming,
        ),
        (
            "chunked",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
            SessionReadError::UnsupportedFraming,
        ),
        (
            "oversized",
            b"HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\n",
            SessionReadError::Oversized,
        ),
        (
            "duplicate length",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            SessionReadError::DuplicateContentLength,
        ),
        (
            "truncated",
            b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n{}",
            SessionReadError::Truncated,
        ),
        (
            "trailing bytes",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra",
            SessionReadError::TrailingBytes,
        ),
        (
            "not json",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n<html",
            SessionReadError::Malformed("not JSON"),
        ),
        (
            "no state",
            b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n{\"status\":1}",
            SessionReadError::Malformed("no state"),
        ),
    ];
    for (name, raw, expected) in cases {
        let harness = Harness::new(constant(raw));
        assert_eq!(
            reader_at(harness.addr).read().unwrap_err(),
            expected,
            "{name}"
        );
        assert_eq!(harness.finish().posts.len(), 0, "{name}: no POST");
    }
}
