//! Host-side proof for the injectable socket-binding seam.
//!
//! The seam is the one place where a gate socket gets bound to a network. These
//! tests bind a loopback listener, inject a **fake binder** that models the OS
//! ("this handle is not the network carrying the target", "the generation
//! changed after this socket connected"), and then assert on what the *server*
//! observed — connections and POST bodies — not on what the client returned.
//!
//! What is not proven here: the real `android_setsocknetwork` call on a device.
//! That needs a phone, a real Wi-Fi network object and a captured
//! `Network.getNetworkHandle()`; see `reports/android-hardened-payment/`.

#![cfg(all(feature = "cdk", feature = "persistence", target_os = "linux"))]

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use socket2::Socket;

use tollgate_client_core::{
    bound_transport::{
        AmbiguousReason, BoundTarget, BoundVerifiedBytesTransport, Limits, NetworkContext,
        RefusalReason, SendOutcome, VerifiedBytes,
    },
    capped_purchase::{SessionPhase, SessionReadError, SessionReader, SESSION_PATH},
    net_bind::{BindError, BoundGet, GetError, LostNetwork, NetworkBinder, NetworkSelection},
};

const TOKEN: &[u8] = b"cashuBo2F0gCE6GibberishTokenBytesGoHere4Q";
const SESSION_NONE: &str = r#"{"status":1,"mac":"aa:bb:cc:dd:ee:ff","state":"none"}"#;

// ------------------------------------------------------------- fake binder ---

/// A stand-in for the platform network object.
///
/// * `bind` models `android_setsocknetwork`: it accepts the socket only while
///   the injected handle is the network that carries the target.
/// * `link_intact` models the generation/link recheck at each boundary. The
///   counter makes "the network disappeared at the k-th state check" exactly
///   reproducible instead of a race.
#[derive(Debug)]
struct FakeBinder {
    handle: u64,
    generation: AtomicU64,
    bind_calls: AtomicUsize,
    refuse_bind: AtomicBool,
    intact_checks: AtomicUsize,
    intact_allow: AtomicUsize,
}

impl FakeBinder {
    fn new(handle: u64) -> Arc<Self> {
        Arc::new(Self {
            handle,
            generation: AtomicU64::new(7),
            bind_calls: AtomicUsize::new(0),
            refuse_bind: AtomicBool::new(false),
            intact_checks: AtomicUsize::new(0),
            intact_allow: AtomicUsize::new(usize::MAX),
        })
    }

    /// The selected network is not the one the OS would route the target over
    /// (the captive-Wi-Fi / cellular-default case): the platform call refuses.
    fn refuse_wrong_network(&self) {
        self.refuse_bind.store(true, Ordering::SeqCst);
    }

    /// Answer `checks` state checks as intact, then report the network gone.
    fn lose_after(&self, checks: usize) {
        self.intact_allow.store(checks, Ordering::SeqCst);
    }

    fn bind_calls(&self) -> usize {
        self.bind_calls.load(Ordering::SeqCst)
    }
}

impl NetworkBinder for FakeBinder {
    fn bind(&self, _socket: &Socket) -> Result<(), BindError> {
        self.bind_calls.fetch_add(1, Ordering::SeqCst);
        if self.refuse_bind.load(Ordering::SeqCst) {
            return Err(BindError::Network {
                kind: "fake-network",
                detail: "selected network does not carry the target".into(),
            });
        }
        Ok(())
    }

    fn link_intact(&self) -> bool {
        let seen = self.intact_checks.fetch_add(1, Ordering::SeqCst) + 1;
        seen <= self.intact_allow.load(Ordering::SeqCst)
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn kind(&self) -> &'static str {
        "fake-network"
    }

    fn lost(&self) -> LostNetwork {
        LostNetwork::Generation {
            kind: "fake-network",
            generation: self.generation.load(Ordering::SeqCst),
        }
    }
}

// ---------------------------------------------------------------- harness ----

#[derive(Default, Debug)]
struct Report {
    connections: usize,
    methods: Vec<String>,
    posts: Vec<Vec<u8>>,
}

struct Harness {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    rx: mpsc::Receiver<Report>,
    join: Option<thread::JoinHandle<()>>,
}

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
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

impl Harness {
    fn new() -> Self {
        Self::with_handler(|request: &[u8]| {
            if request.starts_with(b"GET /session-state") {
                response(200, SESSION_NONE)
            } else {
                response(200, "advert")
            }
        })
    }

    fn with_handler<F>(handler: F) -> Self
    where
        F: Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_in = stop.clone();
        let (tx, rx) = mpsc::channel();
        let join = thread::spawn(move || {
            let mut report = Report::default();
            while !stop_in.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, peer)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        assert_eq!(peer.ip(), IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
                        report.connections += 1;
                        let request = read_request(&mut stream);
                        let method: String = String::from_utf8_lossy(&request)
                            .split("\r\n")
                            .next()
                            .unwrap_or_default()
                            .to_string();
                        if request.starts_with(b"POST ") {
                            report.posts.push(request.clone());
                        }
                        report.methods.push(method);
                        let raw = handler(&request);
                        let _ = stream.write_all(&raw);
                        let _ = stream.flush();
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

fn context() -> NetworkContext {
    NetworkContext::new("lo", IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)).unwrap()
}

fn limits() -> Limits {
    Limits {
        connect: Duration::from_millis(500),
        read: Duration::from_millis(400),
        write: Duration::from_millis(500),
        total: Duration::from_millis(1500),
        max_response_bytes: 4096,
        max_header_bytes: 2048,
    }
}

fn selection(binder: Arc<FakeBinder>) -> NetworkSelection {
    NetworkSelection::with(context(), binder)
}

fn post(sel: &NetworkSelection, addr: SocketAddr) -> SendOutcome {
    let target = BoundTarget::new_http(addr, "/").unwrap();
    let transport =
        BoundVerifiedBytesTransport::with_binder(target, context(), limits(), sel.binder().clone());
    let bytes = VerifiedBytes::new(TOKEN.to_vec()).unwrap();
    transport.send(&bytes)
}

// ------------------------------------------------------------------ tests ----

/// One binder, four request shapes: advert GET, session GET, the payment POST,
/// and the post-grant session probe. Every socket must be created on the seam.
#[test]
fn the_same_seam_carries_advert_get_session_get_post_and_probe() {
    let harness = Harness::new();
    let binder = FakeBinder::new(0x1234_5678_9abc_def0);
    let sel = selection(binder.clone());

    // 1. advert GET
    let advert = BoundGet::new(sel.clone(), limits())
        .read(&BoundTarget::new_http(harness.addr, "/").unwrap())
        .expect("advert GET through the seam");
    assert_eq!(advert.status, 200);
    assert_eq!(advert.body, b"advert".to_vec());

    // 2. session-state GET (the preflight baseline)
    let state = SessionReader::with_selection(
        BoundTarget::new_http(harness.addr, SESSION_PATH).unwrap(),
        context(),
        limits(),
        sel.clone(),
    )
    .read()
    .expect("session GET through the seam");
    assert_eq!(state.state, SessionPhase::Inactive);

    // 3. the payment POST
    match post(&sel, harness.addr) {
        SendOutcome::Complete { status, .. } => assert_eq!(status, 200),
        other => panic!("expected Complete, got {other:?}"),
    }

    // 4. the post-grant probe: the same GET path, same seam
    BoundGet::new(sel.clone(), limits())
        .read(&BoundTarget::new_http(harness.addr, SESSION_PATH).unwrap())
        .expect("post-grant probe through the seam");

    let report = harness.finish();
    assert_eq!(report.connections, 4, "{:?}", report.methods);
    assert_eq!(
        report.methods,
        vec![
            "GET / HTTP/1.1",
            "GET /session-state HTTP/1.1",
            "POST / HTTP/1.1",
            "GET /session-state HTTP/1.1",
        ]
    );
    assert_eq!(report.posts.len(), 1);
    assert_eq!(
        binder.bind_calls(),
        4,
        "every request path went through the injected binder"
    );
}

/// The wrong network is selected (cellular while the target is on captive Wi-Fi,
/// or a handle that was revoked): nothing may leave the process. The server is
/// the witness, so a refusal that still connected would fail here.
#[test]
fn a_refused_network_bind_sends_nothing_on_any_path() {
    let harness = Harness::new();
    let binder = FakeBinder::new(0xdead_beef_0000_0001);
    binder.refuse_wrong_network();
    let sel = selection(binder.clone());

    let advert = BoundGet::new(sel.clone(), limits())
        .read(&BoundTarget::new_http(harness.addr, "/").unwrap());
    assert!(
        matches!(
            advert,
            Err(GetError::RefusedBeforeSend(
                RefusalReason::NetworkBindingFailed { .. }
            ))
        ),
        "{advert:?}"
    );

    let session = SessionReader::with_selection(
        BoundTarget::new_http(harness.addr, SESSION_PATH).unwrap(),
        context(),
        limits(),
        sel.clone(),
    )
    .read();
    assert_eq!(
        session,
        Err(SessionReadError::NetworkLost),
        "a refused binding is never read as an empty session"
    );

    match post(&sel, harness.addr) {
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::NetworkBindingFailed { .. },
        } => {}
        other => panic!("expected a network-bind refusal, got {other:?}"),
    }

    let report = harness.finish();
    assert_eq!(report.connections, 0, "no socket may reach the gate");
    assert!(report.posts.is_empty(), "zero POST bodies");
    assert_eq!(
        binder.bind_calls(),
        3,
        "all three paths consulted the binder"
    );
}

/// The network generation is lost after the socket connected but before the
/// write boundary: the send is refused and the server sees no POST.
#[test]
fn a_network_lost_before_the_write_boundary_refuses_the_post() {
    let harness = Harness::new();
    let binder = FakeBinder::new(0x0000_0000_0000_00aa);
    binder.lose_after(0); // the post-connect check already sees the network gone
    let sel = selection(binder);

    let outcome = post(&sel, harness.addr);
    assert_eq!(
        outcome,
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::NetworkGenerationLost {
                kind: "fake-network",
                generation: 7,
            }
        }
    );
    assert!(!outcome.may_have_been_sent());

    let report = harness.finish();
    assert_eq!(report.connections, 1, "the socket had already connected");
    assert!(report.posts.is_empty(), "no request byte was written");
}

/// The same loss discovered *after* the write is ambiguous: the gate may have
/// acted on the token. It must never be reported as a refusal or a completion.
#[test]
fn a_network_lost_after_the_write_is_ambiguous() {
    let harness = Harness::new();
    let binder = FakeBinder::new(0x0000_0000_0000_00bb);
    binder.lose_after(1); // intact at the pre-write check, gone after the write
    let sel = selection(binder);

    let outcome = post(&sel, harness.addr);
    assert_eq!(
        outcome,
        SendOutcome::Ambiguous {
            reason: AmbiguousReason::NetworkGenerationLost {
                kind: "fake-network",
                generation: 7,
            }
        }
    );
    assert!(outcome.may_have_been_sent());

    let report = harness.finish();
    assert_eq!(report.posts.len(), 1, "the body did reach the gate");
}

/// A read path must fail closed for the same reason, with a typed error rather
/// than a "no session" answer that would authorise a purchase.
#[test]
fn a_network_lost_before_the_read_is_typed_not_an_empty_session() {
    // Lost before the write: the socket connected, nothing was written.
    let harness = Harness::new();
    let binder = FakeBinder::new(0x0000_0000_0000_00cc);
    binder.lose_after(0);

    let reader = SessionReader::with_selection(
        BoundTarget::new_http(harness.addr, SESSION_PATH).unwrap(),
        context(),
        limits(),
        selection(binder),
    );
    assert_eq!(reader.read(), Err(SessionReadError::NetworkLost));

    let report = harness.finish();
    assert_eq!(report.connections, 1);
    assert!(
        report.methods.iter().all(|m| m.is_empty()),
        "no request byte was written, got {:?}",
        report.methods
    );

    // Lost after the write: the answer is still not usable, and it is still not
    // "inactive" — that would be a licence to spend.
    let harness = Harness::new();
    let binder = FakeBinder::new(0x0000_0000_0000_00cd);
    binder.lose_after(1);
    let reader = SessionReader::with_selection(
        BoundTarget::new_http(harness.addr, SESSION_PATH).unwrap(),
        context(),
        limits(),
        selection(binder),
    );
    assert_eq!(reader.read(), Err(SessionReadError::NetworkLost));
    let report = harness.finish();
    assert_eq!(report.methods.len(), 1, "the GET was written");
}

/// Regression: the Linux default is still SO_BINDTODEVICE on the context's
/// interface — the seam must not soften it into an unbound socket.
#[test]
fn the_linux_default_selection_still_binds_the_device() {
    let harness = Harness::new();
    let sel = NetworkSelection::default_for(context());
    assert_eq!(sel.binder().kind(), "bindtodevice");

    match post(&sel, harness.addr) {
        SendOutcome::Complete { status, .. } => assert_eq!(status, 200),
        other => panic!("expected Complete on a bound socket, got {other:?}"),
    }

    // A missing interface keeps failing closed, before any connection.
    let missing = NetworkSelection::default_for(
        NetworkContext::new("nosuchif0", IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)).unwrap(),
    );
    match post(&missing, harness.addr) {
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::InterfaceBindingFailed(_),
        } => {}
        other => panic!("expected an interface bind refusal, got {other:?}"),
    }

    let report = harness.finish();
    assert_eq!(report.connections, 1, "only the bound attempt connected");
}

/// The advert read as production will use it: `describe_gate_bound` prices the
/// offer it fetched through the seam, and refuses with no socket when the binder
/// cannot attach one.
#[test]
fn the_advert_get_path_prices_the_body_and_refuses_without_a_network() {
    const ADVERT: &str = r#"{"kind":10021,"tags":[["metric","milliseconds"],["step_size","1000"],["price_per_step","cashu","1","sat","https://mint.example","1"]]}"#;
    let advert = ADVERT.to_string();
    let handler = {
        let advert = advert.clone();
        move |request: &[u8]| -> Vec<u8> {
            let _ = request;
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                advert.len(),
                advert
            )
            .into_bytes()
        }
    };
    let harness = Harness::with_handler(handler);
    let binder = FakeBinder::new(0x1111_2222_3333_4444);
    let sel = selection(binder.clone());

    let caps = tollgate_client_core::policy::Caps {
        preferred_unit: "sat".into(),
        max_cost_per_minute: 120.0,
        daily_budget: 100.0,
        spent_today: 0.0,
        switch_margin: 0.20,
    };
    let target = BoundTarget::new_http(harness.addr, "/").unwrap();
    let described = tollgate_client_core::runner::describe_gate_bound(
        &sel,
        &target,
        limits(),
        &caps,
        &std::collections::HashMap::new(),
    )
    .expect("advert priced");
    assert!(described.contains("\"offers\":["), "{described}");
    assert!(
        described.contains(&format!("http://{}/", harness.addr)),
        "the priced offer names the endpoint it was fetched from: {described}"
    );
    let report = harness.finish();
    assert_eq!(report.connections, 1);
    assert_eq!(binder.bind_calls(), 1, "the advert GET used the seam");

    // Same call, wrong network: an error and no socket at all.
    let harness = Harness::with_handler(|_| Vec::new());
    let binder = FakeBinder::new(0x5555_6666_7777_8888);
    binder.refuse_wrong_network();
    let sel = selection(binder);
    let target = BoundTarget::new_http(harness.addr, "/").unwrap();
    assert!(tollgate_client_core::runner::describe_gate_bound(
        &sel,
        &target,
        limits(),
        &caps,
        &std::collections::HashMap::new(),
    )
    .is_err());
    assert_eq!(harness.finish().connections, 0);
}

/// What an Android build does today: no `Network` handle has been injected, so
/// the default selection is `NoNetwork` and every path refuses. This is the
/// contract the follow-up card has to fill in — not a silent unbound fallback.
#[test]
fn no_injected_android_network_refuses_every_path() {
    let harness = Harness::new();
    let sel = NetworkSelection::with(
        context(),
        Arc::new(tollgate_client_core::net_bind::NoNetwork::new(
            "no Android Network handle injected",
        )),
    );
    assert_eq!(sel.binder().kind(), "no-network");

    let advert = BoundGet::new(sel.clone(), limits())
        .read(&BoundTarget::new_http(harness.addr, "/").unwrap());
    assert!(
        matches!(
            advert,
            Err(GetError::RefusedBeforeSend(
                RefusalReason::NetworkBindingFailed { .. }
            ))
        ),
        "{advert:?}"
    );
    assert_eq!(
        SessionReader::with_selection(
            BoundTarget::new_http(harness.addr, SESSION_PATH).unwrap(),
            context(),
            limits(),
            sel.clone(),
        )
        .read(),
        Err(SessionReadError::NetworkLost)
    );
    match post(&sel, harness.addr) {
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::NetworkBindingFailed { .. },
        } => {}
        other => panic!("expected a network-binding refusal, got {other:?}"),
    }
    assert_eq!(harness.finish().connections, 0);
}

/// The Android binder's host-observable contract, without a device.
///
/// Two things are provable here and nothing more:
/// * `NETWORK_UNSPECIFIED` (handle 0) is not a network — `android_setsocknetwork`
///   would *clear* a binding, which is the exact unbound fallback this seam
///   forbids, so construction refuses it;
/// * on a build without libandroid the platform call is unavailable, and the
///   binder fails closed instead of degrading to an unbound socket.
///
/// **Not proven here:** that a real `Network.getNetworkHandle()` attaches the
/// socket on a device, and that the link callback fires. That needs a phone.
#[test]
fn the_android_binder_refuses_unspecified_and_fails_closed_on_host() {
    use tollgate_client_core::net_bind::{AndroidNetworkBinder, NetworkLink};

    assert!(
        AndroidNetworkBinder::new(0, NetworkLink::new(1)).is_err(),
        "NETWORK_UNSPECIFIED must never be accepted as a network"
    );

    let link = NetworkLink::new(0xabc);
    let binder =
        AndroidNetworkBinder::new(0x1234_5678_9abc_def0, link.clone()).expect("valid handle");
    assert_eq!(binder.kind(), "android-setsocknetwork");
    assert_eq!(binder.generation(), 0xabc);
    assert!(binder.link_intact());
    link.mark_lost();
    assert!(!binder.link_intact(), "a lost link stays lost");

    // A socket is created but never attaches: the host has no libandroid, so the
    // call is unavailable and the seam must refuse rather than continue unbound.
    let harness = Harness::new();
    let sel = NetworkSelection::with(context(), Arc::new(binder));
    assert_eq!(
        post(&sel, harness.addr),
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::NetworkBindingFailed {
                kind: "android-setsocknetwork",
                detail: "android_setsocknetwork is not available on this build target".into(),
            }
        }
    );
    assert_eq!(
        harness.finish().connections,
        0,
        "no socket reached the gate"
    );
}

/// `NetworkSelection::android_network` is the seam's public entry point: a
/// handle plus its link, no ambient state.
#[test]
fn the_android_entry_point_carries_a_handle_and_its_link() {
    use tollgate_client_core::net_bind::{NetworkLink, NetworkSelection};
    let link = NetworkLink::new(42);
    let sel = NetworkSelection::android_network(context(), 0xfeed_face, link.clone())
        .expect("handle accepted");
    assert_eq!(sel.binder().kind(), "android-setsocknetwork");
    assert_eq!(sel.binder().generation(), 42);
    assert!(sel.binder().link_intact());
    link.mark_lost();
    assert!(!sel.binder().link_intact());
    assert!(NetworkSelection::android_network(context(), 0, link).is_err());
}
