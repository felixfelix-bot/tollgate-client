//! Bound, single-attempt, plain-HTTP transport for already-verified token bytes.
//!
//! This is deliberately **not** a general purpose HTTP client. It carries exactly
//! one POST of one immutable, digest-checked body to one immutable, numeric
//! endpoint from one explicitly bound source/interface, and it reports a
//! three-way outcome that lets the caller separate "nothing was sent" from
//! "something may have been sent and we do not know what happened":
//!
//! * [`crate::bound_transport::SendOutcome::RefusedBeforeSend`] — no request byte reached the socket.
//!   Safe to treat as an unattempted send.
//! * [`crate::bound_transport::SendOutcome::Ambiguous`] — the request may have been (partially or fully)
//!   delivered. Never retried here, never retried by a caller on this evidence.
//! * [`crate::bound_transport::SendOutcome::Complete`] — one request was written and one strictly framed
//!   2xx response was read back. That is *not* proof the gate granted access.
//!
//! Policy that is intentionally absent:
//!
//! * no HTTPS/TLS (the v1 gate is plain HTTP; mints are a separate transport),
//! * no DNS, no proxy environment, no absolute-form request target,
//! * no redirect following — every `3xx` is [`crate::bound_transport::AmbiguousReason::Redirect`], and no
//!   byte of the token body is ever forwarded to `Location`,
//! * no retry, no reconnect, no resend after a partial write,
//! * no request body transformation: [`crate::bound_transport::VerifiedBytes`] can only be built from
//!   bytes the caller already verified, and it is written verbatim.
//!
//! Deferred (see `docs/client/v1-client-contract.md`): mapping an outcome onto
//! `crate::validated_send::VerifiedBytesTransport` / durable send ownership,
//! session-evidence capture, and post-send route re-verification are caller
//! (orchestration) concerns and are not implemented here.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

/// Largest token body this transport will transmit. Pre-funded Cashu notes are
/// small; anything near this bound is a programming error upstream.
pub const MAX_VERIFIED_BYTES: usize = 1024 * 1024;

/// Longest accepted fixed request path.
pub const MAX_PATH_BYTES: usize = 512;

/// Linux `IFNAMSIZ` minus the trailing NUL.
const MAX_INTERFACE_BYTES: usize = 15;

/// An immutable numeric endpoint: `http://<ip>:<port><path>`.
///
/// There is no hostname, no scheme selector and no port derivation, so DNS
/// rebinding and scheme downgrade are structurally impossible rather than
/// policy-checked at send time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundTarget {
    addr: SocketAddr,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// A zero or otherwise non-fixed port.
    UnspecifiedPort,
    /// `0.0.0.0`, `::`, multicast or broadcast destinations.
    NonUnicastAddress,
    PathNotAbsolute,
    PathTooLong,
    PathHasIllegalByte,
}

impl BoundTarget {
    /// The only constructor: a numeric address plus a fixed absolute path.
    pub fn new_http(addr: SocketAddr, path: impl Into<String>) -> Result<Self, TargetError> {
        if addr.port() == 0 {
            return Err(TargetError::UnspecifiedPort);
        }
        if addr.ip().is_unspecified() || addr.ip().is_multicast() {
            return Err(TargetError::NonUnicastAddress);
        }
        let path = path.into();
        if !path.starts_with('/') {
            return Err(TargetError::PathNotAbsolute);
        }
        if path.len() > MAX_PATH_BYTES {
            return Err(TargetError::PathTooLong);
        }
        if path
            .bytes()
            .any(|b| b <= 0x20 || b == 0x7f || b == b'#' || b == b'?')
        {
            return Err(TargetError::PathHasIllegalByte);
        }
        Ok(Self { addr, path })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Fixed at `http` by construction; never HTTPS.
    pub fn scheme(&self) -> &'static str {
        "http"
    }
}

/// Which device and which local address the request must leave from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkContext {
    interface: String,
    source: IpAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextError {
    MissingInterface,
    InterfaceTooLong,
    InterfaceHasIllegalByte,
    UnspecifiedSource,
    MulticastSource,
}

impl NetworkContext {
    /// Both fields are required and validated present; there is no default
    /// interface and no wildcard source, so "binding unavailable" can never
    /// silently degrade into "unbound".
    pub fn new(interface: impl Into<String>, source: IpAddr) -> Result<Self, ContextError> {
        let interface = interface.into();
        if interface.is_empty() {
            return Err(ContextError::MissingInterface);
        }
        if interface.len() > MAX_INTERFACE_BYTES {
            return Err(ContextError::InterfaceTooLong);
        }
        if interface
            .bytes()
            .any(|b| b == 0 || b == b'/' || b.is_ascii_whitespace())
        {
            return Err(ContextError::InterfaceHasIllegalByte);
        }
        if source.is_unspecified() {
            return Err(ContextError::UnspecifiedSource);
        }
        if source.is_multicast() {
            return Err(ContextError::MulticastSource);
        }
        Ok(Self { interface, source })
    }

    pub fn interface(&self) -> &str {
        &self.interface
    }

    pub fn source(&self) -> IpAddr {
        self.source
    }
}

/// Bytes the caller has already verified (structurally valid, digest-checked).
///
/// The wrapper has no mutating accessor: what is sent is exactly what was
/// verified, with no re-encoding, splitting or compression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBytes {
    body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BytesError {
    Empty,
    TooLarge,
}

impl VerifiedBytes {
    pub fn new(body: Vec<u8>) -> Result<Self, BytesError> {
        if body.is_empty() {
            return Err(BytesError::Empty);
        }
        if body.len() > MAX_VERIFIED_BYTES {
            return Err(BytesError::TooLarge);
        }
        Ok(Self { body })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.body
    }

    pub fn len(&self) -> usize {
        self.body.len()
    }

    pub fn is_empty(&self) -> bool {
        self.body.is_empty()
    }
}

/// Explicit deadlines and size bounds. There are no unbounded reads/writes and
/// no inherited OS defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub connect: Duration,
    pub read: Duration,
    pub write: Duration,
    /// Whole-request wall clock bound, covering connect + write + read.
    pub total: Duration,
    pub max_response_bytes: usize,
    pub max_header_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            read: Duration::from_secs(3),
            write: Duration::from_secs(3),
            total: Duration::from_secs(8),
            max_response_bytes: 64 * 1024,
            max_header_bytes: 8 * 1024,
        }
    }
}

/// Why a request was abandoned **before** any request byte was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    InvalidContext(&'static str),
    AddressFamilyMismatch {
        target: IpAddr,
        source: IpAddr,
    },
    InterfaceBindingFailed(String),
    InterfaceVanished(String),
    SourceBindFailed(String),
    ConnectTimeout,
    ConnectFailed(String),
    SourceAddressMismatch {
        expected: IpAddr,
        actual: IpAddr,
    },
    PeerAddressMismatch {
        expected: SocketAddr,
        actual: SocketAddr,
    },
}

/// Why the outcome of an attempted send is unknown. Every one of these means
/// the gate may have consumed the token; none of them is a retry signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmbiguousReason {
    /// Any 3xx: `Location` is never followed and never receives the body.
    Redirect {
        status: u16,
    },
    /// Non-2xx, non-3xx status. Refusals are observations, not non-consumption.
    UnexpectedStatus(u16),
    StatusLineMalformed,
    MalformedHeader(&'static str),
    HeaderTooLarge,
    MissingFraming,
    ConflictingFraming,
    DuplicateContentLength,
    DuplicateTransferEncoding,
    UnsupportedTransferEncoding,
    MalformedChunked(&'static str),
    Truncated,
    Oversized,
    /// More body bytes arrived than the declared framing allows.
    TrailingBytes,
    ConnectionClosedBeforeResponse,
    ReadTimeout,
    ReadFailed(String),
    WriteTimeout,
    WriteFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Complete { status: u16, body: Vec<u8> },
    Ambiguous { reason: AmbiguousReason },
    RefusedBeforeSend { reason: RefusalReason },
}

impl SendOutcome {
    pub fn is_complete(&self) -> bool {
        matches!(self, SendOutcome::Complete { .. })
    }

    /// True unless the request provably never left this process.
    pub fn may_have_been_sent(&self) -> bool {
        !matches!(self, SendOutcome::RefusedBeforeSend { .. })
    }
}

fn refused(reason: RefusalReason) -> SendOutcome {
    SendOutcome::RefusedBeforeSend { reason }
}

fn ambiguous(reason: AmbiguousReason) -> SendOutcome {
    SendOutcome::Ambiguous { reason }
}

/// One immutable endpoint + one immutable binding + one immutable byte body.
///
/// `send` takes `&self` and holds no interior mutability: a second call sends a
/// second, independent request. There is no retry/backoff/redirect state to get
/// wrong, and nothing to reconfigure between verification and write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundVerifiedBytesTransport {
    target: BoundTarget,
    context: NetworkContext,
    limits: Limits,
}

impl BoundVerifiedBytesTransport {
    pub fn new(target: BoundTarget, context: NetworkContext, limits: Limits) -> Self {
        Self {
            target,
            context,
            limits,
        }
    }

    pub fn target(&self) -> &BoundTarget {
        &self.target
    }

    pub fn context(&self) -> &NetworkContext {
        &self.context
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Send the verified bytes exactly once. Single attempt; there is no code
    /// path that reconnects or rewrites. Any error from the write itself is
    /// reported as [`SendOutcome::Ambiguous`] even when it may have failed on
    /// the first byte, because the syscall cannot tell us how much left.
    pub fn send(&self, bytes: &VerifiedBytes) -> SendOutcome {
        let deadline = Instant::now() + self.limits.total;

        // Re-derive the binding invariants at the boundary rather than trusting
        // construction: this is the same check the caller must make, done where
        // it actually matters.
        if self.context.interface.is_empty() {
            return refused(RefusalReason::InvalidContext("interface absent"));
        }
        if self.context.source.is_unspecified() {
            return refused(RefusalReason::InvalidContext("source address absent"));
        }
        if self.context.source.is_ipv4() != self.target.addr.is_ipv4() {
            return refused(RefusalReason::AddressFamilyMismatch {
                target: self.target.addr.ip(),
                source: self.context.source,
            });
        }

        // Nothing to send must never be turned into an empty POST.
        if bytes.is_empty() {
            return refused(RefusalReason::InvalidContext("empty body"));
        }

        let request = self.build_request(bytes);

        let domain = if self.target.addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let sock = match Socket::new(domain, Type::STREAM, Some(Protocol::TCP)) {
            Ok(s) => s,
            Err(e) => return refused(RefusalReason::ConnectFailed(e.to_string())),
        };
        let _ = sock.set_tcp_nodelay(true);

        // Interface binding must succeed; there is no unbound fallback.
        if let Err(e) = sock.bind_device(Some(self.context.interface.as_bytes())) {
            return refused(RefusalReason::InterfaceBindingFailed(e.to_string()));
        }
        if let Err(e) = sock.bind(&SocketAddr::new(self.context.source, 0).into()) {
            return refused(RefusalReason::SourceBindFailed(e.to_string()));
        }
        match sock.connect_timeout(&self.target.addr.into(), self.limits.connect) {
            Ok(()) => {}
            Err(e)
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock =>
            {
                return refused(RefusalReason::ConnectTimeout)
            }
            Err(e) => return refused(RefusalReason::ConnectFailed(e.to_string())),
        }

        let stream: TcpStream = sock.into();

        // The socket must actually be on the bound source before any byte is
        // written; a silently re-routed source is a refusal, not a warning.
        match stream.local_addr() {
            Ok(local) if local.ip() == self.context.source => {}
            Ok(local) => {
                return refused(RefusalReason::SourceAddressMismatch {
                    expected: self.context.source,
                    actual: local.ip(),
                })
            }
            Err(e) => return refused(RefusalReason::SourceBindFailed(e.to_string())),
        }
        match stream.peer_addr() {
            Ok(peer) if peer == self.target.addr => {}
            Ok(peer) => {
                return refused(RefusalReason::PeerAddressMismatch {
                    expected: self.target.addr,
                    actual: peer,
                })
            }
            Err(e) => return refused(RefusalReason::ConnectFailed(e.to_string())),
        }
        // Best-effort post-connect recheck: interface disappearance before the
        // write aborts. After the write this is only ever ambiguous.
        if !std::path::Path::new(&format!("/sys/class/net/{}", self.context.interface)).exists() {
            return refused(RefusalReason::InterfaceVanished(
                self.context.interface.clone(),
            ));
        }

        if let Err(e) = stream.set_write_timeout(Some(self.limits.write)) {
            return refused(RefusalReason::ConnectFailed(e.to_string()));
        }
        if let Err(e) = stream.set_read_timeout(Some(self.limits.read)) {
            return refused(RefusalReason::ConnectFailed(e.to_string()));
        }
        if Instant::now() >= deadline {
            return refused(RefusalReason::ConnectTimeout);
        }

        // ---- irreversible boundary: exactly one write_all, no retry ----
        if let Err(e) = (&stream).write_all(&request) {
            return ambiguous(match e.kind() {
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                    AmbiguousReason::WriteTimeout
                }
                _ => AmbiguousReason::WriteFailed(e.to_string()),
            });
        }

        match read_response(&stream, self.limits, deadline) {
            Ok((status, body)) => SendOutcome::Complete { status, body },
            Err(reason) => ambiguous(reason),
        }
    }

    fn build_request(&self, bytes: &VerifiedBytes) -> Vec<u8> {
        let head = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             User-Agent: tollgate-client-core/bound-transport\r\n\
             Accept: */*\r\n\
             Accept-Encoding: identity\r\n\
             Content-Type: text/plain\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n",
            path = self.target.path,
            host = self.target.addr,
            len = bytes.len(),
        );
        let mut request = Vec::with_capacity(head.len() + bytes.len());
        request.extend_from_slice(head.as_bytes());
        request.extend_from_slice(bytes.as_slice());
        request
    }
}

fn read_response(
    stream: &TcpStream,
    limits: Limits,
    deadline: Instant,
) -> Result<(u16, Vec<u8>), AmbiguousReason> {
    let mut conn = Conn {
        stream,
        buf: Vec::with_capacity(1024),
        pos: 0,
        deadline,
        read_timeout: limits.read,
    };

    // ---- status line + headers ----
    let head_end = loop {
        if let Some(i) = find(conn.buffered(), b"\r\n\r\n") {
            break i;
        }
        if conn.buffered().len() > limits.max_header_bytes {
            return Err(AmbiguousReason::HeaderTooLarge);
        }
        if conn.fill()? == 0 {
            return Err(if conn.buffered().is_empty() {
                AmbiguousReason::ConnectionClosedBeforeResponse
            } else {
                AmbiguousReason::Truncated
            });
        }
    };
    if head_end > limits.max_header_bytes {
        return Err(AmbiguousReason::HeaderTooLarge);
    }
    let head = conn.buffered()[..head_end].to_vec();
    conn.consume(head_end + 4);

    let (status, headers) = parse_head(&head)?;

    if (300..400).contains(&status) {
        // Never follow, never forward the body, never treat as success.
        return Err(AmbiguousReason::Redirect { status });
    }
    if !(200..300).contains(&status) {
        return Err(AmbiguousReason::UnexpectedStatus(status));
    }
    if status == 204 {
        return Ok((status, Vec::new()));
    }

    // ---- framing ----
    let content_lengths: Vec<&str> = headers
        .iter()
        .filter(|(k, _)| k == "content-length")
        .map(|(_, v)| v.as_str())
        .collect();
    let transfer_encodings: Vec<&str> = headers
        .iter()
        .filter(|(k, _)| k == "transfer-encoding")
        .map(|(_, v)| v.as_str())
        .collect();

    if !content_lengths.is_empty() && !transfer_encodings.is_empty() {
        return Err(AmbiguousReason::ConflictingFraming);
    }
    if content_lengths.len() > 1 {
        return Err(AmbiguousReason::DuplicateContentLength);
    }
    if transfer_encodings.len() > 1 {
        return Err(AmbiguousReason::DuplicateTransferEncoding);
    }

    let leftover_len = conn.buffered().len();

    if let Some(raw) = content_lengths.first() {
        let raw = raw.trim();
        if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AmbiguousReason::MalformedHeader("content-length"));
        }
        let declared = raw
            .parse::<usize>()
            .map_err(|_| AmbiguousReason::MalformedHeader("content-length"))?;
        if declared > limits.max_response_bytes {
            return Err(AmbiguousReason::Oversized);
        }
        if leftover_len > declared {
            return Err(AmbiguousReason::TrailingBytes);
        }
        let mut body = Vec::with_capacity(declared);
        conn.take_exact(declared, &mut body)?;
        return Ok((status, body));
    }

    if let Some(raw) = transfer_encodings.first() {
        let tokens: Vec<&str> = raw
            .split(',')
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.len() != 1 || !tokens[0].eq_ignore_ascii_case("chunked") {
            return Err(AmbiguousReason::UnsupportedTransferEncoding);
        }
        let body = conn.read_chunked(limits.max_response_bytes)?;
        return Ok((status, body));
    }

    // Neither Content-Length nor chunked: refuse rather than read to EOF, so a
    // dropped connection cannot masquerade as a short but complete body.
    Err(AmbiguousReason::MissingFraming)
}

/// Parse `HTTP/1.x <code> <reason>` plus headers, lower-casing names and
/// rejecting anything that is not a plain `name: value` line.
fn parse_head(head: &[u8]) -> Result<(u16, Vec<(String, String)>), AmbiguousReason> {
    let text = std::str::from_utf8(head).map_err(|_| AmbiguousReason::StatusLineMalformed)?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or(AmbiguousReason::StatusLineMalformed)?;
    if status_line.contains(['\r', '\n']) || !status_line.starts_with("HTTP/1.") {
        return Err(AmbiguousReason::StatusLineMalformed);
    }
    let mut words = status_line.split_whitespace();
    let _version = words.next();
    let code = words.next().ok_or(AmbiguousReason::StatusLineMalformed)?;
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AmbiguousReason::StatusLineMalformed);
    }
    let status: u16 = code
        .parse()
        .map_err(|_| AmbiguousReason::StatusLineMalformed)?;
    if !(100..600).contains(&status) {
        return Err(AmbiguousReason::StatusLineMalformed);
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        // Obsolete line folding, bare CR/LF and empty names are all refusals.
        if line.starts_with([' ', '\t']) || line.contains(['\r', '\n']) {
            return Err(AmbiguousReason::MalformedHeader("folding"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(AmbiguousReason::MalformedHeader("no colon"))?;
        if name.is_empty()
            || name
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        {
            return Err(AmbiguousReason::MalformedHeader("bad name"));
        }
        headers.push((
            name.to_ascii_lowercase(),
            value.trim_matches(|c| c == ' ' || c == '\t').to_string(),
        ));
    }
    Ok((status, headers))
}

struct Conn<'a> {
    stream: &'a TcpStream,
    buf: Vec<u8>,
    pos: usize,
    deadline: Instant,
    read_timeout: Duration,
}

impl<'a> Conn<'a> {
    fn buffered(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    fn consume(&mut self, n: usize) {
        self.pos += n;
        if self.pos >= self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
    }

    /// Returns the number of bytes added, 0 on clean EOF.
    fn fill(&mut self) -> Result<usize, AmbiguousReason> {
        let mut chunk = [0u8; 4096];
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AmbiguousReason::ReadTimeout);
            }
            let timeout = self.read_timeout.min(remaining);
            self.stream
                .set_read_timeout(Some(timeout))
                .map_err(|e| AmbiguousReason::ReadFailed(e.to_string()))?;
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(0),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e)
                    if e.kind() == io::ErrorKind::TimedOut
                        || e.kind() == io::ErrorKind::WouldBlock =>
                {
                    if Instant::now() >= self.deadline {
                        return Err(AmbiguousReason::ReadTimeout);
                    }
                    continue;
                }
                Err(e) => return Err(AmbiguousReason::ReadFailed(e.to_string())),
            }
        }
    }

    fn take_exact(&mut self, n: usize, out: &mut Vec<u8>) -> Result<(), AmbiguousReason> {
        let mut remaining = n;
        while remaining > 0 {
            if self.buffered().is_empty() && self.fill()? == 0 {
                return Err(AmbiguousReason::Truncated);
            }
            let take = self.buffered().len().min(remaining);
            let end = self.pos + take;
            out.extend_from_slice(&self.buf[self.pos..end]);
            self.consume(take);
            remaining -= take;
        }
        Ok(())
    }

    /// One CRLF-terminated line, without the CRLF.
    fn read_line(&mut self, max: usize) -> Result<String, AmbiguousReason> {
        loop {
            if let Some(i) = find(self.buffered(), b"\r\n") {
                if i > max {
                    return Err(AmbiguousReason::MalformedChunked("line too long"));
                }
                let line = self.buf[self.pos..self.pos + i].to_vec();
                self.consume(i + 2);
                return String::from_utf8(line)
                    .map_err(|_| AmbiguousReason::MalformedChunked("non-ascii line"));
            }
            if self.buffered().len() > max {
                return Err(AmbiguousReason::MalformedChunked("line too long"));
            }
            if self.fill()? == 0 {
                return Err(AmbiguousReason::Truncated);
            }
        }
    }

    fn read_chunked(&mut self, max: usize) -> Result<Vec<u8>, AmbiguousReason> {
        let mut body = Vec::new();
        loop {
            let line = self.read_line(1024)?;
            let size_token = line.split(';').next().unwrap_or("").trim();
            if size_token.is_empty() || !size_token.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(AmbiguousReason::MalformedChunked("bad chunk size"));
            }
            let size = usize::from_str_radix(size_token, 16)
                .map_err(|_| AmbiguousReason::MalformedChunked("bad chunk size"))?;
            if size == 0 {
                // Trailer section: header lines until the terminating CRLF.
                loop {
                    let trailer = self.read_line(8192)?;
                    if trailer.is_empty() {
                        return Ok(body);
                    }
                    if !trailer.contains(':') {
                        return Err(AmbiguousReason::MalformedChunked("bad trailer"));
                    }
                }
            }
            if body.len() + size > max {
                return Err(AmbiguousReason::Oversized);
            }
            self.take_exact(size, &mut body)?;
            let terminator = self.read_line(16)?;
            if !terminator.is_empty() {
                return Err(AmbiguousReason::MalformedChunked("missing chunk CRLF"));
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
