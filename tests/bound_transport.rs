//! Local-socket tests for the bound transport. No network, no router, no funds.
//!
//! Every test binds a loopback `TcpListener` and asserts on what the *server*
//! observed (connection count, byte count, peer address), not just on the
//! client's return value — a transport that "returns Complete" while sending
//! twice, or while sending to a redirect target, fails here.

#![cfg(any(target_os = "linux", target_os = "android"))]

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use tollgate_client_core::bound_transport::{
    AmbiguousReason, BoundTarget, BoundVerifiedBytesTransport, BytesError, ContextError, Limits,
    NetworkContext, RefusalReason, SendOutcome, TargetError, VerifiedBytes, MAX_VERIFIED_BYTES,
};

// ---------------------------------------------------------------- harness ----

type Handler = Arc<dyn Fn(&mut TcpStream) -> Vec<u8> + Send + Sync>;

#[derive(Default, Debug)]
struct Report {
    connections: usize,
    peer_ips: Vec<IpAddr>,
    requests: Vec<Vec<u8>>,
}

struct Harness {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    rx: mpsc::Receiver<Report>,
    join: Option<thread::JoinHandle<()>>,
}

impl Harness {
    fn new<F>(handler: F) -> Self
    where
        F: Fn(&mut TcpStream) -> Vec<u8> + Send + Sync + 'static,
    {
        Harness::on(("127.0.0.1", 0), handler)
    }

    fn on<F, A>(bind: A, handler: F) -> Self
    where
        F: Fn(&mut TcpStream) -> Vec<u8> + Send + Sync + 'static,
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
                        report.connections += 1;
                        report.peer_ips.push(peer.ip());
                        let req = handler(&mut stream);
                        report.requests.push(req);
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

    fn target(&self, context: &NetworkContext, path: &str) -> BoundTarget {
        let _ = context;
        BoundTarget::new_http(self.addr, path).unwrap()
    }

    /// Stop accepting, let a short settle window elapse, and collect observations.
    fn finish(mut self) -> Report {
        thread::sleep(Duration::from_millis(120));
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
        self.rx.recv().expect("harness report")
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Read one complete request (headers + declared body), tolerating a peer that
/// dies mid-request. Returns exactly the bytes the server received.
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

fn respond(raw: &'static [u8]) -> impl Fn(&mut TcpStream) -> Vec<u8> + Send + Sync + 'static {
    move |stream| {
        let req = read_request(stream);
        let _ = stream.write_all(raw);
        let _ = stream.flush();
        req
    }
}

fn context(source: &str) -> NetworkContext {
    NetworkContext::new("lo", source.parse().unwrap()).unwrap()
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

fn send(harness: &Harness, context: &NetworkContext, path: &str, body: &[u8]) -> SendOutcome {
    let target = harness.target(context, path);
    let transport = BoundVerifiedBytesTransport::new(target, context.clone(), limits());
    let bytes = VerifiedBytes::new(body.to_vec()).unwrap();
    transport.send(&bytes)
}

/// The request body the server actually received, if it received a full request.
fn received_body(request: &[u8]) -> Option<Vec<u8>> {
    let i = find(request, b"\r\n\r\n")?;
    Some(request[i + 4..].to_vec())
}

const TOKEN: &[u8] = b"cashuBo2F0gCE6GibberishTokenBytesGoHere4Q";

// ------------------------------------------------------------------ tests ----

#[test]
fn exact_bytes_are_written_once_and_a_200_is_complete() {
    let harness = Harness::new(respond(
        b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world",
    ));
    // 127.0.0.2 is a real loopback address: proves the source bind took effect.
    let ctx = context("127.0.0.2");
    let outcome = send(&harness, &ctx, "/", TOKEN);
    let report = harness.finish();

    match outcome {
        SendOutcome::Complete { status, body } => {
            assert_eq!(status, 200);
            assert_eq!(body, b"hello world");
        }
        other => panic!("expected Complete, got {other:?}"),
    }

    assert_eq!(report.connections, 1, "exactly one TCP connection");
    assert_eq!(
        report.peer_ips,
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))]
    );

    let request = &report.requests[0];
    // Byte-exact: header block plus the verified body, nothing appended.
    let head_end = find(request, b"\r\n\r\n").unwrap() + 4;
    assert_eq!(request.len(), head_end + TOKEN.len(), "exact byte count");
    assert_eq!(received_body(request).unwrap(), TOKEN);
    assert_eq!(
        request
            .windows(4)
            .filter(|window| *window == b"POST")
            .count(),
        1,
        "exactly one POST written"
    );
    assert!(request.starts_with(b"POST / HTTP/1.1\r\n"));
    assert!(!String::from_utf8_lossy(request).contains("Transfer-Encoding"));
}

#[test]
fn a_302_is_ambiguous_and_the_location_listener_receives_nothing() {
    let decoy = Harness::new(respond(
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\ndecoy",
    ));

    let location = format!("http://127.0.0.1:{}/", decoy.addr.port());
    let raw: &'static [u8] = Box::leak(
        format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
            .into_bytes()
            .into_boxed_slice(),
    );
    let harness = Harness::new(respond(raw));

    let ctx = context("127.0.0.1");
    let outcome = send(&harness, &ctx, "/", TOKEN);
    let first = harness.finish();
    let second = decoy.finish();

    assert_eq!(
        outcome,
        SendOutcome::Ambiguous {
            reason: AmbiguousReason::Redirect { status: 302 }
        }
    );
    assert_eq!(first.connections, 1, "the redirect is not retried");
    assert_eq!(second.connections, 0, "Location never receives a POST");
    assert!(second.requests.is_empty());
}

#[test]
fn all_redirect_codes_are_ambiguous() {
    for code in [301u16, 303, 307, 308] {
        let raw: &'static [u8] = Box::leak(
            format!("HTTP/1.1 {code} Moved\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\n\r\n")
                .into_bytes()
                .into_boxed_slice(),
        );
        let harness = Harness::new(respond(raw));
        let ctx = context("127.0.0.1");
        let outcome = send(&harness, &ctx, "/", TOKEN);
        let report = harness.finish();
        assert_eq!(
            outcome,
            SendOutcome::Ambiguous {
                reason: AmbiguousReason::Redirect { status: code }
            },
            "3xx {code} must be ambiguous"
        );
        assert_eq!(report.connections, 1, "no retry for 3xx {code}");
    }
}

#[test]
fn oversized_response_is_ambiguous() {
    // Declared length above max_response_bytes (4096).
    let harness = Harness::new(respond(b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\n\r\n"));
    let ctx = context("127.0.0.1");
    let outcome = send(&harness, &ctx, "/", TOKEN);
    let report = harness.finish();
    assert_eq!(
        outcome,
        SendOutcome::Ambiguous {
            reason: AmbiguousReason::Oversized
        }
    );
    assert_eq!(report.connections, 1);
    assert_eq!(received_body(&report.requests[0]).unwrap(), TOKEN);
}

#[test]
fn truncated_response_is_ambiguous_and_not_retried() {
    let harness = Harness::new(respond(
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort",
    ));
    let ctx = context("127.0.0.1");
    let outcome = send(&harness, &ctx, "/", TOKEN);
    let report = harness.finish();
    assert_eq!(
        outcome,
        SendOutcome::Ambiguous {
            reason: AmbiguousReason::Truncated
        }
    );
    assert_eq!(report.connections, 1, "no reconnect after ambiguous read");
}

#[test]
fn valid_chunked_response_is_complete() {
    let harness = Harness::new(respond(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    ));
    let ctx = context("127.0.0.1");
    let outcome = send(&harness, &ctx, "/", TOKEN);
    harness.finish();
    assert_eq!(
        outcome,
        SendOutcome::Complete {
            status: 200,
            body: b"hello world".to_vec()
        }
    );
}

#[test]
fn malformed_chunked_responses_are_ambiguous() {
    let cases: Vec<(&str, &'static [u8], AmbiguousReason)> = vec![
        (
            "bad chunk size",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nabc\r\n0\r\n\r\n",
            AmbiguousReason::MalformedChunked("bad chunk size"),
        ),
        (
            "missing chunk terminator",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcX\r\n0\r\n\r\n",
            AmbiguousReason::MalformedChunked("missing chunk CRLF"),
        ),
        (
            "truncated mid-chunk",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\nabc",
            AmbiguousReason::Truncated,
        ),
        (
            "no last-chunk",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n",
            AmbiguousReason::Truncated,
        ),
    ];
    for (name, raw, expected) in cases {
        let harness = Harness::new(respond(raw));
        let ctx = context("127.0.0.1");
        let outcome = send(&harness, &ctx, "/", TOKEN);
        harness.finish();
        assert_eq!(
            outcome,
            SendOutcome::Ambiguous { reason: expected },
            "{name}"
        );
    }
}

#[test]
fn ambiguous_framing_is_refused_as_ambiguous() {
    let cases: Vec<(&str, &'static [u8], AmbiguousReason)> = vec![
        (
            "no framing at all",
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nhello",
            AmbiguousReason::MissingFraming,
        ),
        (
            "duplicate transfer-encoding",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            AmbiguousReason::DuplicateTransferEncoding,
        ),
        (
            "unsupported transfer-encoding",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n",
            AmbiguousReason::UnsupportedTransferEncoding,
        ),
        (
            "chunked plus content-length",
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            AmbiguousReason::ConflictingFraming,
        ),
        (
            "duplicate content-length",
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\nabc",
            AmbiguousReason::DuplicateContentLength,
        ),
        (
            "non-numeric content-length",
            b"HTTP/1.1 200 OK\r\nContent-Length: 3x\r\n\r\nabc",
            AmbiguousReason::MalformedHeader("content-length"),
        ),
        (
            "garbage status line",
            b"ICY 200 OK\r\nContent-Length: 3\r\n\r\nabc",
            AmbiguousReason::StatusLineMalformed,
        ),
        (
            "folded header",
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n continued\r\n\r\nabc",
            AmbiguousReason::MalformedHeader("folding"),
        ),
    ];
    for (name, raw, expected) in cases {
        let harness = Harness::new(respond(raw));
        let ctx = context("127.0.0.1");
        let outcome = send(&harness, &ctx, "/", TOKEN);
        harness.finish();
        assert_eq!(
            outcome,
            SendOutcome::Ambiguous { reason: expected },
            "{name}"
        );
    }
}

/// A peer that claims a `usize::MAX` chunk size must not be able to wrap the
/// running total past the size guard. Before `checked_add`, this summed to a
/// small number in release (guard silently disarmed) and panicked in debug —
/// and both happened *after* the token bytes had already been written, which
/// escapes the three-way outcome entirely. It must be `Ambiguous::Oversized`.
///
/// The overflow needs a non-empty body first: with `body.len() == 0` the bare
/// comparison already refuses. One small chunk followed by `ffffffffffffffff`
/// is the shape that wrapped.
#[test]
fn chunk_size_overflow_is_oversized_not_a_panic() {
    let harness = Harness::new(respond(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\nffffffffffffffff\r\n",
    ));
    let ctx = context("127.0.0.1");
    let outcome = send(&harness, &ctx, "/", TOKEN);
    harness.finish();
    assert_eq!(
        outcome,
        SendOutcome::Ambiguous {
            reason: AmbiguousReason::Oversized
        },
        "an unrepresentable total is oversized, never a panic and never a pass"
    );
}

#[test]
fn non_2xx_is_ambiguous_not_a_refusal() {
    for code in [400u16, 402, 409, 500, 503] {
        let raw: &'static [u8] = Box::leak(
            format!("HTTP/1.1 {code} X\r\nContent-Length: 2\r\n\r\nno")
                .into_bytes()
                .into_boxed_slice(),
        );
        let harness = Harness::new(respond(raw));
        let ctx = context("127.0.0.1");
        let outcome = send(&harness, &ctx, "/", TOKEN);
        harness.finish();
        assert_eq!(
            outcome,
            SendOutcome::Ambiguous {
                reason: AmbiguousReason::UnexpectedStatus(code)
            }
        );
        // A refusal after the token was written is still potential consumption.
        assert!(outcome.may_have_been_sent());
    }
}

#[test]
fn connect_timeout_is_refused_before_send() {
    // A listener with a backlog of 1 that never accepts: further connects hang,
    // which is exactly the "gate is unreachable" case we must not mistake for a
    // send.
    let s = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .unwrap();
    s.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0).into())
        .unwrap();
    s.listen(1).unwrap();
    let port = s.local_addr().unwrap().as_socket().unwrap().port();

    let mut fillers = Vec::new();
    for _ in 0..8 {
        let filler = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .unwrap();
        let target: socket2::SockAddr =
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port).into();
        let _ = filler.connect_timeout(&target, Duration::from_millis(50));
        fillers.push(filler);
    }

    let ctx = context("127.0.0.1");
    let target =
        BoundTarget::new_http(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port), "/").unwrap();
    let transport = BoundVerifiedBytesTransport::new(target, ctx, limits());
    let bytes = VerifiedBytes::new(TOKEN.to_vec()).unwrap();

    let started = Instant::now();
    let outcome = transport.send(&bytes);
    let elapsed = started.elapsed();

    assert_eq!(
        outcome,
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::ConnectTimeout
        }
    );
    assert!(!outcome.may_have_been_sent());
    assert!(
        elapsed >= Duration::from_millis(450),
        "connect deadline honoured, elapsed {elapsed:?}"
    );
    drop(fillers);
}

#[test]
fn wrong_source_address_is_refused_before_send() {
    let harness = Harness::new(respond(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"));
    // 192.0.2.0/24 is TEST-NET-1: never assigned to a local interface.
    let ctx = NetworkContext::new("lo", "192.0.2.123".parse().unwrap()).unwrap();
    let outcome = send(&harness, &ctx, "/", TOKEN);
    let report = harness.finish();

    match outcome {
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::SourceBindFailed(_),
        } => {}
        other => panic!("expected source bind refusal, got {other:?}"),
    }
    assert_eq!(report.connections, 0, "zero connections on a bind failure");
    assert!(report.requests.is_empty());
}

#[test]
fn missing_interface_fails_closed_before_send() {
    let harness = Harness::new(respond(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"));
    let ctx = NetworkContext::new("nosuchif0", "127.0.0.1".parse().unwrap()).unwrap();
    let outcome = send(&harness, &ctx, "/", TOKEN);
    let report = harness.finish();

    match outcome {
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::InterfaceBindingFailed(_),
        } => {}
        other => panic!("expected interface bind refusal, got {other:?}"),
    }
    assert_eq!(report.connections, 0);
}

#[test]
fn address_family_mismatch_is_refused_before_send() {
    let harness = Harness::new(respond(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"));
    let ctx = context("127.0.0.1");
    let target = BoundTarget::new_http(
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), harness.addr.port()),
        "/",
    )
    .unwrap();
    let transport = BoundVerifiedBytesTransport::new(target, ctx, limits());
    let bytes = VerifiedBytes::new(TOKEN.to_vec()).unwrap();
    let outcome = transport.send(&bytes);
    let report = harness.finish();
    assert_eq!(
        outcome,
        SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::AddressFamilyMismatch {
                target: IpAddr::V6(Ipv6Addr::LOCALHOST),
                source: IpAddr::V4(Ipv4Addr::LOCALHOST),
            }
        }
    );
    assert_eq!(report.connections, 0);
}

#[test]
fn a_peer_that_vanishes_mid_write_is_ambiguous_and_not_retried() {
    let body = vec![b'A'; MAX_VERIFIED_BYTES];
    let harness = Harness::new(move |stream| {
        // Read a little, then reset the connection so the client's write fails
        // part-way through the body.
        let mut head = [0u8; 512];
        let _ = stream.read(&mut head);
        // Dropping with unread bytes still queued makes Linux send RST, so the
        // client's remaining body write fails part-way.
        vec![]
    });
    let ctx = context("127.0.0.1");
    let outcome = send(&harness, &ctx, "/", &body);
    let report = harness.finish();

    assert!(outcome.may_have_been_sent(), "{outcome:?}");
    match outcome {
        SendOutcome::Ambiguous { .. } => {}
        other => panic!("expected Ambiguous after a partial write, got {other:?}"),
    }
    assert_eq!(report.connections, 1, "a partial write is never retried");
}

#[test]
fn read_timeout_is_ambiguous() {
    let harness = Harness::new(|stream| {
        let req = read_request(stream);
        // Hold the response past the transport's whole-request deadline.
        thread::sleep(Duration::from_millis(2500));
        req
    });
    let ctx = context("127.0.0.1");
    let started = Instant::now();
    let outcome = send(&harness, &ctx, "/", TOKEN);
    let elapsed = started.elapsed();
    let report = harness.finish();

    assert_eq!(
        outcome,
        SendOutcome::Ambiguous {
            reason: AmbiguousReason::ReadTimeout
        }
    );
    assert_eq!(report.connections, 1);
    assert!(
        elapsed >= Duration::from_millis(1400) && elapsed < Duration::from_millis(2200),
        "total deadline enforced, elapsed {elapsed:?}"
    );
}

#[test]
fn ipv6_loopback_binding_works() {
    let bind: SocketAddr = (Ipv6Addr::LOCALHOST, 0).into();
    let listener = match TcpListener::bind(bind) {
        Ok(l) => l,
        Err(_) => return, // no v6 loopback on this host
    };
    drop(listener);
    let harness = Harness::on(
        bind,
        respond(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
    );
    let ctx = NetworkContext::new("lo", "::1".parse().unwrap()).unwrap();
    let outcome = send(&harness, &ctx, "/", TOKEN);
    let report = harness.finish();
    assert_eq!(
        outcome,
        SendOutcome::Complete {
            status: 200,
            body: b"ok".to_vec()
        }
    );
    assert_eq!(report.connections, 1);
    assert_eq!(report.peer_ips, vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]);
}

#[test]
fn target_and_context_validation() {
    assert_eq!(
        BoundTarget::new_http("127.0.0.1:0".parse().unwrap(), "/"),
        Err(TargetError::UnspecifiedPort)
    );
    assert_eq!(
        BoundTarget::new_http("0.0.0.0:2121".parse().unwrap(), "/"),
        Err(TargetError::NonUnicastAddress)
    );
    assert_eq!(
        BoundTarget::new_http("127.0.0.1:2121".parse().unwrap(), "relative"),
        Err(TargetError::PathNotAbsolute)
    );
    assert_eq!(
        BoundTarget::new_http("127.0.0.1:2121".parse().unwrap(), "/a b"),
        Err(TargetError::PathHasIllegalByte)
    );
    let target = BoundTarget::new_http("127.0.0.1:2121".parse().unwrap(), "/").unwrap();
    assert_eq!(target.scheme(), "http", "scheme is fixed, never https");

    assert_eq!(
        NetworkContext::new("", IpAddr::V4(Ipv4Addr::LOCALHOST)),
        Err(ContextError::MissingInterface)
    );
    assert_eq!(
        NetworkContext::new("lo", IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
        Err(ContextError::UnspecifiedSource)
    );
    assert_eq!(
        NetworkContext::new("eth0/1", IpAddr::V4(Ipv4Addr::LOCALHOST)),
        Err(ContextError::InterfaceHasIllegalByte)
    );

    assert_eq!(VerifiedBytes::new(Vec::new()), Err(BytesError::Empty));
    assert_eq!(
        VerifiedBytes::new(vec![0u8; MAX_VERIFIED_BYTES + 1]),
        Err(BytesError::TooLarge)
    );
    let bytes = VerifiedBytes::new(TOKEN.to_vec()).unwrap();
    assert_eq!(bytes.as_slice(), TOKEN);
    assert_eq!(bytes.len(), TOKEN.len());
}

#[test]
fn one_transport_instance_is_immutable_across_sends() {
    let harness = Harness::new(respond(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"));
    let ctx = context("127.0.0.1");
    let target = harness.target(&ctx, "/pay");
    let transport = BoundVerifiedBytesTransport::new(target.clone(), ctx.clone(), limits());
    let bytes = VerifiedBytes::new(TOKEN.to_vec()).unwrap();

    assert!(transport.send(&bytes).is_complete());
    assert!(transport.send(&bytes).is_complete());
    let report = harness.finish();

    // Two calls, two independent requests: `send` holds no retry state, and the
    // target it was built with cannot be mutated between them.
    assert_eq!(report.connections, 2);
    assert_eq!(transport.target(), &target);
    assert_eq!(transport.context(), &ctx);
    assert_eq!(transport.limits(), limits());
    for request in &report.requests {
        assert_eq!(received_body(request).unwrap(), TOKEN);
    }
}
