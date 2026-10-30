use super::*;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;

#[test]
fn reopened_evidence_never_replaces_records() {
    let dir = PathBuf::from(format!("/dev/shm/capped-wire-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = std::collections::HashSet::new();
    for i in 0..20 {
        let bytes = i.to_string();
        let path = EvidenceSink::open(&dir)
            .unwrap()
            .write("test", bytes.as_bytes())
            .unwrap();
        assert!(
            paths.insert(path.clone()),
            "reopened sink replaced evidence"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn duplicate_json_members_are_rejected() {
    for body in [
        br#"{"state":"none","state":"active"}"#.as_slice(),
        br#"{"state":"none","extra":{"x":1,"x":2}}"#,
        br#"{"state":"none","st\u0061te":"active"}"#,
    ] {
        assert!(parse_session_body(body).is_err());
    }
    assert!(notice_from_body(
        200,
        br#"{"kind":1,"kind":1022,"tags":[["allotment","1"],["metric","milliseconds"]]}"#
    )
    .is_err());
}

#[test]
fn strict_http_status_and_headers() {
    for head in [
        "HTTP/1.99 200 OK",
        "HTTP/1.1\t200 OK",
        "HTTP/1.1 200 OK\r\nX-Test: bad\0value",
    ] {
        assert!(parse_head(head.as_bytes()).is_err(), "accepted {head:?}");
    }
}

#[test]
fn poll_constructor_has_operational_caps() {
    let p = PollPolicy::new(u32::MAX, Duration::MAX);
    assert!(p.attempts <= 16);
    assert!(p.interval <= Duration::from_secs(1));
}

fn reader(addr: SocketAddr) -> SessionReader {
    SessionReader::new(
        BoundTarget::new_http(addr, SESSION_PATH).unwrap(),
        NetworkContext::new("lo", "127.0.0.1".parse().unwrap()).unwrap(),
        Limits::default(),
    )
}

#[test]
fn delayed_trailing_bytes_are_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let reader = reader(listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut request = [0; 1024];
        s.read(&mut request).unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n")
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));
        s.write_all(b"{\"state\":\"none\"}garbage").unwrap();
    });
    assert_eq!(reader.read(), Err(SessionReadError::TrailingBytes));
    server.join().unwrap();
}

#[test]
fn stalled_write_respects_total_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let reader = reader(listener.local_addr().unwrap());
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
    socket.set_send_buffer_size(4096).unwrap();
    socket
        .connect(&listener.local_addr().unwrap().into())
        .unwrap();
    let (_peer, _) = listener.accept().unwrap(); // intentionally never reads
    let stream: TcpStream = socket.into();
    let start = Instant::now();
    assert!(reader
        .write_request(
            &stream,
            &vec![0; 8 * 1024 * 1024],
            start + Duration::from_millis(50)
        )
        .is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn public_zero_poll_policy_is_normalized() {
    let mut reader = reader("127.0.0.1:1".parse().unwrap());
    reader.limits.total = Duration::ZERO;
    let (_, error) = poll_session(
        &reader,
        PollPolicy {
            attempts: 0,
            interval: Duration::MAX,
        },
    );
    assert_eq!(error, Some(SessionReadError::ConnectTimeout));
}

#[test]
fn zero_total_budget_never_connects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut reader = reader(listener.local_addr().unwrap());
    reader.limits.total = Duration::ZERO;
    assert!(reader.read().is_err());
    assert!(listener.accept().is_err(), "connected after total deadline");
}
