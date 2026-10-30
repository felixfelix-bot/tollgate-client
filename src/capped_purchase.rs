//! Capped single-purchase orchestration: the seam that connects the reviewed
//! pieces without weakening any of them.
//!
//! Three parts, in the order the contract requires:
//!
//! 1. [`SessionReader`] — a bounded, read-only `GET /session-state` against the
//!    same numeric [`BoundTarget`] + [`NetworkContext`] + [`Limits`] the payment
//!    POST uses. No redirects, no chunked bodies, bounded EOF verification.
//! 2. [`ObservingTransport`] — a [`VerifiedBytesTransport`] wrapper that forwards
//!    the verified body to the bound transport exactly once and turns its
//!    three-way [`SendOutcome`] into the ledger's [`DeliveryObservation`], keeping
//!    a durable per-operation observation record.
//! 3. [`purchase_once`] — baseline the session, send once through
//!    [`PolicyAdapter::attempt`], poll the session a bounded number of times, and
//!    write one evidence record that never contains bearer bytes.
//!
//! # Why a pre-send refusal is recorded as `Unknown`
//!
//! [`crate::send_state::BudgetStore::send_validated`] commits the irreversible
//! send mark, the full-face-plus-fee reservation and the proof claims in one
//! transaction **before** it calls the transport. By the time
//! [`ObservingTransport`] runs, this process has already spent its right to say
//! "nothing was sent, so nothing is owed".
//!
//! So a [`SendOutcome::RefusedBeforeSend`] is *not* reported as
//! [`DeliveryObservation::RefusalOrMalformed`] — that would tell the ledger the
//! gate answered something, while the honest fact is that this client provably
//! wrote no byte. It maps to [`DeliveryObservation::Unknown`] anyway, because the
//! exposure is already durable and only manual reconciliation can release it.
//! Reporting `Unknown` conservatively retains exposure; the distinction survives
//! in the observation record as `pre_send_refusal: true`, which is exactly the
//! flag the reconciler needs.
//!
//! **Outstanding gap: manual reconciliation.** Nothing here can release that
//! exposure. A `RefusedBeforeSend` (unreachable gate, vanished interface, source
//! bind failure) leaves the note owned and the budget reserved until an operator
//! reconciles against the mint (NUT-07) and the ledger. The mitigations are, in
//! order: a **preflight reachability check** (the baseline session read must
//! succeed before any spend is attempted, so a gate that is down or misrouted is
//! discovered *before* the send mark is committed), a single bounded POST, and an
//! evidence file that separates "no byte was written" from "we do not know".
//!
//! **No verdict establishes access.** A notice plus a later active session is
//! only a gate-reported observation, not purchase attribution or data-plane
//! proof. The protocol has no purchase-bound session generation. Historical
//! delivery on replay remains historical even when the current gate is active.

#[cfg(test)]
#[path = "capped_wire_tests.rs"]
mod wire_tests;

use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cdk::secp256k1::hashes::{sha256, Hash};
use serde::{Serialize, Serializer};
use socket2::{Domain, Protocol, Socket, Type};

use crate::{
    bound_transport::{
        AmbiguousReason, BoundTarget, BoundVerifiedBytesTransport, Limits, NetworkContext,
        RefusalReason, SendOutcome, VerifiedBytes,
    },
    offline_policy::{Enrollment, PolicyAdapter},
    send_state::DeliveryObservation,
    token_vault::{ProtectedTokenVault, VaultRef},
    validated_send::VerifiedBytesTransport,
};

/// The only path this module will ever read a session from.
pub const SESSION_PATH: &str = "/session-state";

/// Longest session answer accepted. The gate's own answer is under 200 bytes.
pub const MAX_SESSION_BODY: usize = 2048;

/// Evidence schema version; a reader must reject anything else.
pub const EVIDENCE_SCHEMA: &str = "tollgate.capped-purchase.evidence.v1";

/// Observation-record schema version, one per forwarded body.
pub const OBSERVATION_SCHEMA: &str = "tollgate.capped-purchase.send-observation.v1";

// --------------------------------------------------------------- session -----

/// The gate's own vocabulary, reduced to what a payer may act on. Anything the
/// client does not recognise stays recognisably unrecognised instead of being
/// folded into "not active" (which would authorise a second purchase) or into
/// "active" (which would hide a real failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// Gate reported `none`; not a data-plane measurement.
    Inactive,
    /// Gate reported `active`; not proof of access or purchase attribution.
    Active,
    /// Any other value, or a missing `state` field.
    Unrecognized,
}

impl SessionPhase {
    /// Exact match on the wire value; there is no case folding and no alias.
    pub fn from_wire(state: &str) -> Self {
        match state {
            "none" => SessionPhase::Inactive,
            "active" => SessionPhase::Active,
            _ => SessionPhase::Unrecognized,
        }
    }

    /// Writes the gate's own token back out, so evidence can be compared with a
    /// `curl /session-state` taken at the same moment.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionPhase::Inactive => "none",
            SessionPhase::Active => "active",
            SessionPhase::Unrecognized => "unrecognized",
        }
    }
}

/// One typed session observation. `raw_digest` binds the record to the exact
/// bytes the gate sent without storing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    pub state: SessionPhase,
    pub mac: Option<String>,
    pub allotment: Option<u64>,
    pub metric: Option<String>,
    pub raw_digest: String,
}

impl SessionState {
    pub fn is_active(&self) -> bool {
        self.state == SessionPhase::Active
    }
}

/// Typed failure of a session read. Every variant means "I have no trustworthy
/// session state", never "the session is not active".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionReadError {
    /// Anything but [`SESSION_PATH`]: refused structurally, no connection opened.
    WrongPath,
    InvalidContext(&'static str),
    AddressFamilyMismatch,
    ConnectFailed(String),
    ConnectTimeout,
    WriteFailed(String),
    ReadFailed(String),
    ReadTimeout,
    /// A 3xx answer. Never followed; the `Location` gets no request.
    Redirect {
        status: u16,
    },
    UnexpectedStatus(u16),
    StatusLineMalformed,
    MalformedHeader(&'static str),
    HeaderTooLarge,
    /// Neither `Content-Length` nor anything else: refused rather than read to
    /// EOF, so a dropped connection cannot look like a short but complete body.
    MissingFraming,
    ConflictingFraming,
    DuplicateContentLength,
    /// `Transfer-Encoding` in any form. This reader understands a declared
    /// `Content-Length` only, so a chunked answer is refused rather than
    /// half-parsed.
    UnsupportedFraming,
    Truncated,
    TrailingBytes,
    Oversized,
    /// A complete body that is not a JSON object with a string `state`.
    Malformed(&'static str),
}

impl SessionReadError {
    /// Stable tag for the evidence file. Never carries gate-supplied text.
    pub fn tag(&self) -> &'static str {
        match self {
            SessionReadError::WrongPath => "wrong_path",
            SessionReadError::InvalidContext(_) => "invalid_context",
            SessionReadError::AddressFamilyMismatch => "address_family_mismatch",
            SessionReadError::ConnectFailed(_) => "connect_failed",
            SessionReadError::ConnectTimeout => "connect_timeout",
            SessionReadError::WriteFailed(_) => "write_failed",
            SessionReadError::ReadFailed(_) => "read_failed",
            SessionReadError::ReadTimeout => "read_timeout",
            SessionReadError::Redirect { .. } => "redirect",
            SessionReadError::UnexpectedStatus(_) => "unexpected_status",
            SessionReadError::StatusLineMalformed => "status_line_malformed",
            SessionReadError::MalformedHeader(_) => "malformed_header",
            SessionReadError::HeaderTooLarge => "header_too_large",
            SessionReadError::MissingFraming => "missing_framing",
            SessionReadError::ConflictingFraming => "conflicting_framing",
            SessionReadError::DuplicateContentLength => "duplicate_content_length",
            SessionReadError::UnsupportedFraming => "unsupported_framing",
            SessionReadError::Truncated => "truncated",
            SessionReadError::TrailingBytes => "trailing_bytes",
            SessionReadError::Oversized => "oversized",
            SessionReadError::Malformed(_) => "malformed",
        }
    }
}

impl std::fmt::Display for SessionReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session read failed: {}", self.tag())
    }
}

impl std::error::Error for SessionReadError {}

/// Bounded, read-only session reader.
///
/// Deliberately a separate module from [`crate::bound_transport`]: this one only
/// ever issues one `GET` of one fixed path from the same numeric endpoint and
/// binding, and it carries no body in either direction. It shares the *style*
/// (numeric target, explicit source/interface, whole-request deadline, strict
/// framing, typed outcome) without touching the reviewed send path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReader {
    target: BoundTarget,
    context: NetworkContext,
    limits: Limits,
}

impl SessionReader {
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

    /// One `GET`, one response, one typed answer. Never retried here.
    pub fn read(&self) -> Result<SessionState, SessionReadError> {
        if self.target.path() != SESSION_PATH {
            return Err(SessionReadError::WrongPath);
        }
        if self.context.interface().is_empty() {
            return Err(SessionReadError::InvalidContext("interface absent"));
        }
        if self.context.source().is_unspecified() {
            return Err(SessionReadError::InvalidContext("source address absent"));
        }
        if self.context.source().is_ipv4() != self.target.addr().is_ipv4() {
            return Err(SessionReadError::AddressFamilyMismatch);
        }

        let deadline = Instant::now() + self.limits.total.min(Duration::from_secs(10));
        let stream = self.connect(deadline)?;

        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             User-Agent: tollgate-client-core/session-reader\r\n\
             Accept: */*\r\n\
             Accept-Encoding: identity\r\n\
             Connection: close\r\n\
             \r\n",
            path = SESSION_PATH,
            host = self.target.addr(),
        );
        self.write_request(&stream, request.as_bytes(), deadline)?;

        let (_status, body) = self.read_response(&stream, deadline)?;
        let raw_digest = sha256::Hash::hash(&body).to_string();
        let (state, allotment, metric) = parse_session_body(&body)?;
        Ok(SessionState {
            state,
            allotment,
            metric,
            mac: session_mac(
                &serde_json::from_slice(&body)
                    .map_err(|_| SessionReadError::Malformed("not JSON"))?,
            )?,
            raw_digest,
        })
    }

    fn write_request(
        &self,
        mut stream: &TcpStream,
        mut bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), SessionReadError> {
        while !bytes.is_empty() {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(self.limits.write);
            if remaining.is_zero() {
                return Err(SessionReadError::WriteFailed("timeout".into()));
            }
            stream
                .set_write_timeout(Some(remaining))
                .map_err(|e| SessionReadError::WriteFailed(e.to_string()))?;
            match stream.write(bytes) {
                Ok(0) => return Err(SessionReadError::WriteFailed("write zero".into())),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(SessionReadError::WriteFailed(e.to_string())),
            }
        }
        Ok(())
    }

    fn connect(&self, deadline: Instant) -> Result<TcpStream, SessionReadError> {
        let domain = if self.target.addr().is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let sock = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
            .map_err(|e| SessionReadError::ConnectFailed(e.to_string()))?;
        let _ = sock.set_tcp_nodelay(true);
        // No unbound fallback: an unusable binding is an error, not a warning.
        sock.bind_device(Some(self.context.interface().as_bytes()))
            .map_err(|e| SessionReadError::ConnectFailed(e.to_string()))?;
        sock.bind(&SocketAddr::new(self.context.source(), 0).into())
            .map_err(|e| SessionReadError::ConnectFailed(e.to_string()))?;
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(self.limits.connect);
        if remaining.is_zero() {
            return Err(SessionReadError::ConnectTimeout);
        }
        match sock.connect_timeout(&self.target.addr().into(), remaining) {
            Ok(()) => {}
            Err(e) if is_timeout(&e) => return Err(SessionReadError::ConnectTimeout),
            Err(e) => return Err(SessionReadError::ConnectFailed(e.to_string())),
        }
        let stream: TcpStream = sock.into();
        // The socket must actually be on the bound source and on the intended
        // peer before a single byte is written.
        match stream.local_addr() {
            Ok(local) if local.ip() == self.context.source() => {}
            Ok(_) => return Err(SessionReadError::InvalidContext("source not bound")),
            Err(e) => return Err(SessionReadError::ConnectFailed(e.to_string())),
        }
        match stream.peer_addr() {
            Ok(peer) if peer == self.target.addr() => {}
            Ok(_) => return Err(SessionReadError::InvalidContext("peer mismatch")),
            Err(e) => return Err(SessionReadError::ConnectFailed(e.to_string())),
        }
        Ok(stream)
    }

    fn read_response(
        &self,
        stream: &TcpStream,
        deadline: Instant,
    ) -> Result<(u16, Vec<u8>), SessionReadError> {
        let mut conn = Conn {
            stream,
            buf: Vec::with_capacity(512),
            pos: 0,
            deadline,
            read_timeout: self.limits.read,
        };

        let head_end = loop {
            if let Some(i) = find(conn.buffered(), b"\r\n\r\n") {
                break i;
            }
            if conn.buffered().len() > self.limits.max_header_bytes {
                return Err(SessionReadError::HeaderTooLarge);
            }
            if conn.fill()? == 0 {
                return Err(if conn.buffered().is_empty() {
                    SessionReadError::ReadFailed("closed before response".into())
                } else {
                    SessionReadError::Truncated
                });
            }
        };
        if head_end > self.limits.max_header_bytes {
            return Err(SessionReadError::HeaderTooLarge);
        }
        let head = conn.buffered()[..head_end].to_vec();
        conn.consume(head_end + 4);
        let (status, headers) = parse_head(&head)?;

        if (300..400).contains(&status) {
            return Err(SessionReadError::Redirect { status });
        }
        if status != 200 {
            return Err(SessionReadError::UnexpectedStatus(status));
        }

        let encodings: Vec<&str> = headers
            .iter()
            .filter(|(k, _)| k == "transfer-encoding")
            .map(|(_, v)| v.as_str())
            .collect();
        if !encodings.is_empty() {
            let lengths = headers
                .iter()
                .filter(|(k, _)| k == "content-length")
                .count();
            return Err(if lengths > 0 {
                SessionReadError::ConflictingFraming
            } else {
                SessionReadError::UnsupportedFraming
            });
        }
        let lengths: Vec<&str> = headers
            .iter()
            .filter(|(k, _)| k == "content-length")
            .map(|(_, v)| v.as_str())
            .collect();
        if lengths.len() > 1 {
            return Err(SessionReadError::DuplicateContentLength);
        }
        let raw = *lengths.first().ok_or(SessionReadError::MissingFraming)?;
        let raw = raw.trim();
        if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SessionReadError::MalformedHeader("content-length"));
        }
        let declared = raw
            .parse::<usize>()
            .map_err(|_| SessionReadError::MalformedHeader("content-length"))?;
        // The bound is the caller's limit *and* this module's own ceiling.
        let max = self.limits.max_response_bytes.min(MAX_SESSION_BODY);
        if declared > max {
            return Err(SessionReadError::Oversized);
        }
        if conn.buffered().len() > declared {
            return Err(SessionReadError::TrailingBytes);
        }
        let mut body = Vec::with_capacity(declared);
        conn.take_exact(declared, &mut body)?;
        // Connection: close is required. Probe for EOF within the same deadline;
        // never accept surplus bytes merely because TCP split them into packets.
        if !conn.buffered().is_empty() || conn.fill()? != 0 {
            return Err(SessionReadError::TrailingBytes);
        }
        Ok((status, body))
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
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

    fn fill(&mut self) -> Result<usize, SessionReadError> {
        let mut chunk = [0u8; 2048];
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || self.read_timeout.is_zero() {
                return Err(SessionReadError::ReadTimeout);
            }
            self.stream
                .set_read_timeout(Some(self.read_timeout.min(remaining)))
                .map_err(|e| SessionReadError::ReadFailed(e.to_string()))?;
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(0),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if is_timeout(&e) => {
                    if Instant::now() >= self.deadline {
                        return Err(SessionReadError::ReadTimeout);
                    }
                    continue;
                }
                Err(e) => return Err(SessionReadError::ReadFailed(e.to_string())),
            }
        }
    }

    fn take_exact(&mut self, n: usize, out: &mut Vec<u8>) -> Result<(), SessionReadError> {
        let mut remaining = n;
        while remaining > 0 {
            if self.buffered().is_empty() && self.fill()? == 0 {
                return Err(SessionReadError::Truncated);
            }
            let take = self.buffered().len().min(remaining);
            let end = self.pos + take;
            out.extend_from_slice(&self.buf[self.pos..end]);
            self.consume(take);
            remaining -= take;
        }
        Ok(())
    }
}

fn parse_head(head: &[u8]) -> Result<(u16, Vec<(String, String)>), SessionReadError> {
    let text = std::str::from_utf8(head).map_err(|_| SessionReadError::StatusLineMalformed)?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or(SessionReadError::StatusLineMalformed)?;
    let mut words = status_line.splitn(3, ' ');
    if !matches!(words.next(), Some("HTTP/1.0" | "HTTP/1.1"))
        || status_line.bytes().any(|b| b < 32 || b == 127)
    {
        return Err(SessionReadError::StatusLineMalformed);
    }
    let code = words.next().ok_or(SessionReadError::StatusLineMalformed)?;
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SessionReadError::StatusLineMalformed);
    }
    let status: u16 = code
        .parse()
        .map_err(|_| SessionReadError::StatusLineMalformed)?;
    if !(100..600).contains(&status) {
        return Err(SessionReadError::StatusLineMalformed);
    }
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        // Obsolete folding and bad names are refusals, not leniency.
        if line.starts_with([' ', '\t']) {
            return Err(SessionReadError::MalformedHeader("folding"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(SessionReadError::MalformedHeader("no colon"))?;
        if name.is_empty()
            || name
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        {
            return Err(SessionReadError::MalformedHeader("bad name"));
        }
        if value.bytes().any(|b| (b < 32 && b != b'\t') || b == 127) {
            return Err(SessionReadError::MalformedHeader("bad value"));
        }
        headers.push((
            name.to_ascii_lowercase(),
            value.trim_matches(|c| c == ' ' || c == '\t').to_string(),
        ));
    }
    Ok((status, headers))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Strict body parse: a JSON object with a string `state`, plus — when present —
/// a non-zero integer `allotment` and a known `metric` (milliseconds or bytes).
///
/// Unknown keys are tolerated because the gate's own answer also carries `status`
/// and `mac` (validated if present). A body without a usable `state` is an
/// error rather than a guessed phase.
// Deserialize recursively rather than through Value's last-key-wins map.
fn strict_json(body: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
    struct Unique(serde_json::Value);
    impl<'de> serde::Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Unique;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON with unique object keys")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> Result<Unique, A::Error> {
                    let mut map = serde_json::Map::new();
                    while let Some(key) = a.next_key::<String>()? {
                        if map.contains_key(&key) {
                            return Err(serde::de::Error::custom("duplicate member"));
                        }
                        map.insert(key, a.next_value::<Unique>()?.0);
                    }
                    Ok(Unique(serde_json::Value::Object(map)))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> Result<Unique, A::Error> {
                    let mut values = Vec::new();
                    while let Some(v) = a.next_element::<Unique>()? {
                        values.push(v.0);
                    }
                    Ok(Unique(serde_json::Value::Array(values)))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                    Ok(Unique(serde_json::Value::Null))
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    serde_json::from_slice::<Unique>(body).map(|v| v.0)
}

fn parse_session_body(
    body: &[u8],
) -> Result<(SessionPhase, Option<u64>, Option<String>), SessionReadError> {
    if body.is_empty() {
        return Err(SessionReadError::Malformed("empty body"));
    }
    let value: serde_json::Value =
        strict_json(body).map_err(|_| SessionReadError::Malformed("not JSON"))?;
    let object = value
        .as_object()
        .ok_or(SessionReadError::Malformed("not an object"))?;
    session_mac(&value)?;
    let state = object
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or(SessionReadError::Malformed("no state"))?;
    let positive = |n: u64| -> Result<u64, SessionReadError> {
        if n == 0 {
            Err(SessionReadError::Malformed("bad allotment"))
        } else {
            Ok(n)
        }
    };
    let allotment = match object.get("allotment") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => Some(positive(
            n.as_u64()
                .ok_or(SessionReadError::Malformed("bad allotment"))?,
        )?),
        Some(serde_json::Value::String(s)) => {
            Some(positive(s.trim().parse::<u64>().map_err(|_| {
                SessionReadError::Malformed("bad allotment")
            })?)?)
        }
        Some(_) => return Err(SessionReadError::Malformed("bad allotment")),
    };
    let metric = match object.get("metric") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) if known_metric(s) => Some(s.clone()),
        Some(_) => return Err(SessionReadError::Malformed("bad metric")),
    };
    Ok((SessionPhase::from_wire(state), allotment, metric))
}

fn known_metric(value: &str) -> bool {
    matches!(value, "milliseconds" | "bytes")
}

fn valid_mac(value: &str) -> bool {
    value.len() == 17
        && value.split(':').count() == 6
        && value
            .split(':')
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn session_mac(value: &serde_json::Value) -> Result<Option<String>, SessionReadError> {
    match value.get("mac") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if valid_mac(s) => Ok(Some(s.to_ascii_lowercase())),
        _ => Err(SessionReadError::Malformed("bad device")),
    }
}

// ----------------------------------------------------------- observation -----

/// A usable kind-1022 grant notice: `200`, `kind: 1022`, one `allotment` tag that
/// parses to a non-zero integer, and one known `metric` tag.
///
/// The notice is evidence that *the gate claims* it granted something. It is not
/// evidence that this device has access — see [`purchase_once`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrantNotice {
    /// Actual address only; the protocol literal `mac` is an identifier type, not an address.
    #[serde(skip)]
    pub device_mac: Option<String>,
    pub allotment: u64,
    pub metric: String,
    pub raw_digest: String,
}

/// Borrowing view of [`SendOutcome`], so classification can be tested and
/// documented without cloning a body.
#[derive(Debug, Clone, Copy)]
pub enum SendOutcomeView<'a> {
    Complete { status: u16, body: &'a [u8] },
    Ambiguous { reason: &'a AmbiguousReason },
    RefusedBeforeSend { reason: &'a RefusalReason },
}

impl<'a> From<&'a SendOutcome> for SendOutcomeView<'a> {
    fn from(outcome: &'a SendOutcome) -> Self {
        match outcome {
            SendOutcome::Complete { status, body } => SendOutcomeView::Complete {
                status: *status,
                body,
            },
            SendOutcome::Ambiguous { reason } => SendOutcomeView::Ambiguous { reason },
            SendOutcome::RefusedBeforeSend { reason } => {
                SendOutcomeView::RefusedBeforeSend { reason }
            }
        }
    }
}

/// The classification of one forwarded body, kept whole: the ledger only needs
/// [`DeliveryObservation`], but the evidence record needs to say *why*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSend {
    pub delivery: DeliveryObservation,
    pub notice: Option<GrantNotice>,
    /// True only for [`SendOutcome::RefusedBeforeSend`]: no request byte reached
    /// the socket, but the irreversible send mark was already committed.
    pub pre_send_refusal: bool,
    pub detail: Option<String>,
}

/// Map a send outcome onto the ledger's vocabulary.
///
/// * `Complete` + `200` + a well-formed kind-1022 notice with allotment **and**
///   metric → [`DeliveryObservation::GrantNotice`].
/// * `Complete` but non-2xx, unparseable, missing a tag, or not kind 1022 →
///   [`DeliveryObservation::RefusalOrMalformed`] (the gate answered; it did not
///   grant).
/// * `Ambiguous` → [`DeliveryObservation::Unknown`] (the request may be in flight
///   at the gate; never a retry signal).
/// * `RefusedBeforeSend` → [`DeliveryObservation::Unknown`], because the
///   irreversible send mark is already committed by `send_validated` before the
///   transport runs; a pre-send refusal therefore conservatively retains
///   exposure. Manual reconciliation is the outstanding gap and the preflight
///   session read in [`purchase_once`] is the mitigation.
pub fn parse_send_outcome(outcome: SendOutcomeView<'_>) -> ParsedSend {
    match outcome {
        SendOutcomeView::Complete { status, body } => match notice_from_body(status, body) {
            Ok(notice) => ParsedSend {
                delivery: DeliveryObservation::GrantNotice,
                notice: Some(notice),
                pre_send_refusal: false,
                detail: None,
            },
            Err(detail) => ParsedSend {
                delivery: DeliveryObservation::RefusalOrMalformed,
                notice: None,
                pre_send_refusal: false,
                detail: Some(detail),
            },
        },
        SendOutcomeView::Ambiguous { reason } => ParsedSend {
            delivery: DeliveryObservation::Unknown,
            notice: None,
            pre_send_refusal: false,
            detail: Some(format!("ambiguous:{reason:?}")),
        },
        SendOutcomeView::RefusedBeforeSend { reason } => ParsedSend {
            delivery: DeliveryObservation::Unknown,
            notice: None,
            pre_send_refusal: true,
            detail: Some(format!("refused_before_send:{reason:?}")),
        },
    }
}

fn notice_from_body(status: u16, body: &[u8]) -> Result<GrantNotice, String> {
    if status != 200 {
        return Err(format!("unexpected_status:{status}"));
    }
    if body.is_empty() {
        return Err("empty_body".into());
    }
    let value: serde_json::Value = strict_json(body).map_err(|_| "body_not_json".to_string())?;
    let object = value.as_object().ok_or("body_not_object")?;
    let kind = object.get("kind").and_then(|v| v.as_u64());
    if kind != Some(1022) {
        return Err(match kind {
            Some(kind) => format!("wrong_kind:{kind}"),
            None => "no_kind".into(),
        });
    }
    let tags = object
        .get("tags")
        .and_then(|v| v.as_array())
        .ok_or("no_tags")?;
    let mut allotment: Option<u64> = None;
    let mut metric: Option<String> = None;
    let mut device_mac = None;
    let mut device_seen = false;
    for tag in tags {
        let pair = tag.as_array().ok_or("tag_not_array")?;
        if pair.len() != 2 {
            return Err("tag_not_pair".into());
        }
        let key = pair[0].as_str().ok_or("tag_key_not_string")?;
        let value = pair[1].as_str().ok_or("tag_value_not_string")?;
        match key {
            "allotment" => {
                if allotment.is_some() {
                    return Err("duplicate_allotment".into());
                }
                // Tags are strings on the wire; a bare number is not the gate's
                // shape and is refused rather than coerced.
                let parsed = value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("bad_allotment")?;
                allotment = Some(parsed);
            }
            "metric" => {
                if metric.is_some() {
                    return Err("duplicate_metric".into());
                }
                if !known_metric(value) {
                    return Err("bad_metric".into());
                }
                metric = Some(value.to_string());
            }
            "device-identifier" => {
                if device_seen {
                    return Err("duplicate_device".into());
                }
                device_seen = true;
                if value == "mac" {
                    // Gate versions emit the identifier TYPE rather than an address.
                } else if valid_mac(value) {
                    device_mac = Some(value.to_ascii_lowercase());
                } else {
                    return Err("bad_device".into());
                }
            }
            _ => {}
        }
    }
    // Both are required: an allotment with no metric cannot become a rate, and a
    // metric with no allotment is not a grant this client could honour.
    Ok(GrantNotice {
        device_mac,
        allotment: allotment.ok_or("no_allotment")?,
        metric: metric.ok_or("no_metric")?,
        raw_digest: sha256::Hash::hash(body).to_string(),
    })
}

/// A transport that can report *why* a body was or was not delivered.
///
/// [`VerifiedBytesTransport`] hands back only a [`DeliveryObservation`], which is
/// all the ledger needs; orchestration also needs the three-way outcome, and the
/// only honest way to get it is from the transport that saw it.
pub trait ObservedTransport {
    fn send_observed(&mut self, body: &[u8]) -> SendOutcome;
}

/// The reviewed bound transport, seen through the observed seam. The body is
/// re-wrapped as [`VerifiedBytes`] — which is what `send_validated` loaded from
/// the vault — and written exactly once; nothing is added, re-encoded or retried.
impl ObservedTransport for BoundVerifiedBytesTransport {
    fn send_observed(&mut self, body: &[u8]) -> SendOutcome {
        match VerifiedBytes::new(body.to_vec()) {
            // Fully qualified: `self.send` would otherwise resolve to this
            // module's `VerifiedBytesTransport` impl right below.
            Ok(bytes) => BoundVerifiedBytesTransport::send(self, &bytes),
            // Unrepresentable locally, so provably nothing was written. Still a
            // `RefusedBeforeSend`; the mapping to `Unknown` happens in
            // `parse_send_outcome` for the reason documented there.
            Err(_) => SendOutcome::RefusedBeforeSend {
                reason: RefusalReason::InvalidContext("body not representable"),
            },
        }
    }
}

/// Durable observation of one forwarded send. Metadata only: no bearer bytes, and
/// no digest of the bearer (the ledger already owns that reference).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservationRecord {
    pub schema: String,
    pub operation: String,
    pub endpoint_digest: String,
    pub network_interface: String,
    pub network_source: String,
    pub delivery: String,
    pub pre_send_refusal: bool,
    pub body_bytes: usize,
    pub notice: Option<GrantNotice>,
    pub detail: Option<String>,
    /// Where the record was durably written, or why it could not be.
    pub persisted: Option<String>,
    pub persist_error: Option<String>,
}

/// A [`VerifiedBytesTransport`] that forwards once, classifies the outcome, and
/// keeps a durable per-operation record.
///
/// `send` takes `&mut self` only because the trait does; the inner transport is
/// never reconfigured, and there is no retry path anywhere in this type. A second
/// call would be a second, independent request — the ledger is what prevents one,
/// not this wrapper.
pub struct ObservingTransport<T> {
    inner: T,
    operation: String,
    endpoint_digest: String,
    network: Option<(String, IpAddr)>,
    sink: Option<EvidenceSink>,
    last: Option<ObservationRecord>,
    sends: usize,
}

impl<T> ObservingTransport<T> {
    /// Without a sink this is the pure classifier used by unit tests. Production
    /// callers use [`ObservingTransport::recording`] so the observation is durable.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            operation: String::new(),
            endpoint_digest: String::new(),
            network: None,
            sink: None,
            last: None,
            sends: 0,
        }
    }

    /// Attach the durable record. `operation` is a label, never a secret.
    pub fn recording(
        inner: T,
        sink: EvidenceSink,
        operation: impl Into<String>,
        endpoint_digest: String,
        network: (String, IpAddr),
    ) -> Self {
        Self {
            inner,
            operation: operation.into(),
            endpoint_digest,
            network: Some(network),
            sink: Some(sink),
            last: None,
            sends: 0,
        }
    }

    /// The record of the last forwarded send, if one happened.
    pub fn last(&self) -> Option<&ObservationRecord> {
        self.last.as_ref()
    }

    /// How many bodies were forwarded. `0` means the ledger replayed a previous
    /// attempt and never reached the transport.
    pub fn sends(&self) -> usize {
        self.sends
    }

    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: ObservedTransport> VerifiedBytesTransport for ObservingTransport<T> {
    /// Forward the verified body exactly once and record what happened.
    ///
    /// The return value is the ledger's view. The full three-way outcome is kept
    /// in [`ObservingTransport::last`] and in the durable record. A failure to
    /// persist the record does **not** change the classification — losing the
    /// note never turns a refusal into a grant — but it is surfaced to the caller
    /// as a note, because a missing record must not look like a clean run.
    fn send(&mut self, body: &[u8]) -> DeliveryObservation {
        self.sends += 1;
        let outcome = self.inner.send_observed(body);
        let parsed = parse_send_outcome((&outcome).into());
        let (interface, source) = match &self.network {
            Some((interface, source)) => (interface.clone(), source.to_string()),
            None => (String::new(), String::new()),
        };
        let mut record = ObservationRecord {
            schema: OBSERVATION_SCHEMA.into(),
            operation: self.operation.clone(),
            endpoint_digest: self.endpoint_digest.clone(),
            network_interface: interface,
            network_source: source,
            delivery: delivery_tag(parsed.delivery).into(),
            pre_send_refusal: parsed.pre_send_refusal,
            body_bytes: body.len(),
            notice: parsed.notice.clone(),
            detail: parsed.detail.clone(),
            persisted: None,
            persist_error: None,
        };
        if let Some(sink) = &self.sink {
            match serde_json::to_vec(&record) {
                Ok(encoded) => match sink.write("send", &encoded) {
                    Ok(path) => record.persisted = Some(path.display().to_string()),
                    Err(e) => record.persist_error = Some(e.to_string()),
                },
                Err(e) => record.persist_error = Some(e.to_string()),
            }
        }
        self.last = Some(record);
        parsed.delivery
    }
}

pub fn delivery_tag(delivery: DeliveryObservation) -> &'static str {
    match delivery {
        DeliveryObservation::Unknown => "unknown",
        DeliveryObservation::GrantNotice => "grant_notice",
        DeliveryObservation::RefusalOrMalformed => "refusal_or_malformed",
    }
}

fn delivery_from_tag(tag: &str) -> Option<DeliveryObservation> {
    Some(match tag {
        "unknown" => DeliveryObservation::Unknown,
        "grant_notice" => DeliveryObservation::GrantNotice,
        "refusal_or_malformed" => DeliveryObservation::RefusalOrMalformed,
        _ => return None,
    })
}

// -------------------------------------------------------------- evidence -----

/// The verdict of one orchestration call. None proves device access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A usable grant notice and context-consistent gate-reported active state.
    /// The legacy Rust variant name does NOT imply purchase attribution.
    GrantAndSessionActive,
    /// A usable grant notice, but the session never became active. Money may be
    /// gone and access was not obtained: this is the case that must reach a human.
    GrantWithoutSession,
    /// The baseline session was already `active`, so nothing was sent.
    SessionAlreadyActive,
    /// The gate answered and the answer was not a usable grant, or this client
    /// refused to attempt (ledger/enrollment/plan refusal).
    Refused,
    /// No trustworthy answer: ambiguous delivery, pre-send refusal, unusable
    /// baseline, unrecognised baseline state, or a session that could not be read.
    Unresolved,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::GrantAndSessionActive => "grant_and_gate_reported_active_unattributed",
            Verdict::GrantWithoutSession => "grant_without_session",
            Verdict::SessionAlreadyActive => "session_already_active",
            Verdict::Refused => "refused",
            Verdict::Unresolved => "unresolved",
        }
    }

    /// This protocol supplies no purchase-bound or data-plane proof of access.
    pub fn reports_access(self) -> bool {
        false
    }
}

impl Serialize for Verdict {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bounded polling parameters. There is no unbounded wait and no "poll until
/// active": a gate that never opens a session is a fact to report, not to spin on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollPolicy {
    pub attempts: u32,
    pub interval: Duration,
}

impl PollPolicy {
    pub fn new(attempts: u32, interval: Duration) -> Self {
        Self {
            attempts: attempts.clamp(1, 16),
            interval: interval.min(Duration::from_secs(1)),
        }
    }
}

impl Default for PollPolicy {
    fn default() -> Self {
        Self {
            attempts: 4,
            interval: Duration::from_millis(250),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceNetwork {
    pub interface: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceSession {
    /// The gate's own token: `none`, `active`, or `unrecognized`.
    pub phase: String,
    pub allotment: Option<u64>,
    pub metric: Option<String>,
    pub raw_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceSend {
    /// True only when the transport actually ran for this call.
    pub attempted: bool,
    /// True when the ledger already owned this operation, so the call was
    /// status-only and nothing was written.
    pub replayed: bool,
    pub delivery: String,
    pub notice: Option<GrantNotice>,
    pub pre_send_refusal: bool,
    pub outcome_detail: Option<String>,
    pub attempt_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceTimestamps {
    pub started_unix: u64,
    pub finished_unix: u64,
    pub elapsed_ms: u64,
}

/// One immutable evidence record per call. Field names are the contract; there
/// are no bearer bytes and no token-derived digest in it. `raw_digest` values are
/// digests of the *gate's* answers, which is what makes a later reconciliation
/// possible without keeping those answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub schema: String,
    pub operation: String,
    pub offer_digest: Option<String>,
    pub payment_endpoint_digest: String,
    pub session_endpoint_digest: String,
    pub network: EvidenceNetwork,
    pub baseline: Option<EvidenceSession>,
    pub baseline_error: Option<String>,
    pub send: EvidenceSend,
    pub post_session: Option<EvidenceSession>,
    pub timestamps: EvidenceTimestamps,
    pub verdict: Verdict,
    pub notes: Vec<String>,
}

/// One durable evidence record, plus where it lives.
#[derive(Debug, Clone)]
pub struct Published {
    pub path: PathBuf,
    pub evidence: Evidence,
}

#[derive(Debug)]
pub enum PurchaseError {
    /// The request is not representable (bad reference, label, endpoints).
    InvalidRequest(&'static str),
    Vault(String),
    Ledger(String),
    Evidence(String),
}

impl std::fmt::Display for PurchaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PurchaseError::InvalidRequest(why) => write!(f, "invalid request: {why}"),
            PurchaseError::Vault(why) => write!(f, "vault: {why}"),
            PurchaseError::Ledger(why) => write!(f, "ledger: {why}"),
            PurchaseError::Evidence(why) => write!(f, "evidence: {why}"),
        }
    }
}

impl std::error::Error for PurchaseError {}

/// A private directory of append-only evidence files.
///
/// The leaf must be a `0700` directory; files are written `0600` under a temporary
/// name, fsynced, linked without replacement, and the directory is fsynced, so a reader never sees a
/// half-written record.
#[derive(Debug, Clone)]
pub struct EvidenceSink {
    dir: PathBuf,
    seq: Arc<AtomicU64>,
}

impl EvidenceSink {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let dir = path.as_ref().to_path_buf();
        if !dir.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "evidence directory must be absolute",
            ));
        }
        std::fs::create_dir_all(&dir)?;
        let meta = std::fs::symlink_metadata(&dir)?;
        if !meta.is_dir() || meta.mode() & 0o7777 != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "evidence directory must be a private 0700 directory",
            ));
        }
        Ok(Self {
            dir,
            seq: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Write one record as `<stem>.<kind>.json`. Collisions across reopened
    /// sinks or processes are retried using atomic no-replace publication.
    /// A replay cannot overwrite the evidence of the attempt it replays.
    fn write(&self, kind: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        loop {
            let seq = self.seq.fetch_add(1, Ordering::SeqCst);
            let stem = format!("{}-{}-{}", std::process::id(), unix_now(), seq);
            let final_path = self.dir.join(format!("{stem}.{kind}.json"));
            let tmp_path = self.dir.join(format!(".{stem}.{kind}.tmp"));
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp_path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            };
            let result = (|| {
                file.write_all(bytes)?;
                file.sync_all()?;
                // hard_link atomically publishes a complete inode and NEVER
                // replaces a destination, even across reopened/concurrent sinks.
                std::fs::hard_link(&tmp_path, &final_path)
            })();
            drop(file);
            std::fs::remove_file(&tmp_path)?;
            match result {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
                Ok(()) => {
                    std::fs::File::open(&self.dir)?.sync_all()?;
                    return Ok(final_path);
                }
            }
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn digest_of(bytes: &[u8]) -> String {
    sha256::Hash::hash(bytes).to_string()
}

fn endpoint_digest(target: &BoundTarget) -> String {
    digest_of(format!("{}://{}{}", target.scheme(), target.addr(), target.path()).as_bytes())
}

fn parse_reference(reference: &str) -> Result<VaultRef, PurchaseError> {
    if reference.len() != 64 {
        return Err(PurchaseError::InvalidRequest("reference is not 32 bytes"));
    }
    let mut digest = [0u8; 32];
    for (out, pair) in digest.iter_mut().zip(reference.as_bytes().chunks_exact(2)) {
        let nibble = |b: u8| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(PurchaseError::InvalidRequest(
                "reference is not lowercase hex",
            )),
        };
        *out = nibble(pair[0])? * 16 + nibble(pair[1])?;
    }
    Ok(VaultRef::from_digest(digest))
}

// ----------------------------------------------------------- orchestration ----

/// Everything one capped purchase needs, with no ambient configuration.
pub struct PurchaseRequest<'a> {
    /// The enrollment the caller believes is installed; the adapter re-checks it.
    pub enrollment: &'a Enrollment,
    /// Mint snapshot bytes; the adapter re-hashes them against the enrollment.
    pub snapshot_bytes: Vec<u8>,
    /// The ledger this adapter was enrolled against. Read-only, for replay status.
    pub ledger_path: PathBuf,
    pub vault_path: PathBuf,
    /// Hex SHA-256 of the vault object holding the verified token.
    pub reference: String,
    /// Ledger operation identity. Also the replay key: one operation, one send.
    pub operation: String,
    pub day: u64,
    /// Caller-supplied time for the enrollment freshness window.
    pub now: u64,
    /// Digest of the offer this purchase answers, if the caller has one.
    pub offer_digest: Option<String>,
    pub pay_target: BoundTarget,
    pub session_target: BoundTarget,
    pub context: NetworkContext,
    pub limits: Limits,
    pub poll: PollPolicy,
}

fn session_evidence(state: &SessionState) -> EvidenceSession {
    EvidenceSession {
        phase: state.state.as_str().to_string(),
        allotment: state.allotment,
        metric: state.metric.clone(),
        raw_digest: state.raw_digest.clone(),
    }
}

fn poll_session(
    reader: &SessionReader,
    policy: PollPolicy,
) -> (Option<SessionState>, Option<SessionReadError>) {
    // Normalize here too: public struct literals must not bypass the caps.
    let policy = PollPolicy::new(policy.attempts, policy.interval);
    let mut last: Option<SessionState> = None;
    let mut error: Option<SessionReadError> = None;
    for attempt in 0..policy.attempts {
        if attempt > 0 && !policy.interval.is_zero() {
            std::thread::sleep(policy.interval);
        }
        match reader.read() {
            Ok(state) => {
                let active = state.is_active();
                last = Some(state);
                error = None;
                if active {
                    break;
                }
            }
            // A failed read does not erase the last observation; it is reported
            // alongside it.
            Err(e) => error = Some(e),
        }
    }
    (last, error)
}

/// The ledger's own view of this operation, if it already owns one.
///
/// Opening a second read-only handle is deliberate: the adapter owns its
/// connection, and going through the reviewed [`crate::budget_store::BudgetStore`]
/// API keeps this module out of the ledger by any other route.
///
/// `None` means "this operation has never attempted a send". A returned value —
/// including [`DeliveryObservation::Unknown`] — means the irreversible mark
/// already exists and this call may only observe.
fn ledger_delivery(
    path: &Path,
    operation: &str,
) -> Result<Option<DeliveryObservation>, PurchaseError> {
    let store = crate::budget_store::BudgetStore::open(path)
        .map_err(|e| PurchaseError::Ledger(format!("ledger unavailable: {e:?}")))?;
    match store.send_status(operation) {
        Ok(status) => Ok(Some(status.delivery)),
        // No row for this operation: nothing has been sent yet.
        Err(_) => Ok(None),
    }
}

/// Classify what the ledger knows plus what the session says.
///
/// Classifications are gate claims, never purchase-attributed access.
fn judge(delivery: DeliveryObservation, active: bool, notes: &mut Vec<String>) -> Verdict {
    match delivery {
        DeliveryObservation::GrantNotice if active => Verdict::GrantAndSessionActive,
        DeliveryObservation::GrantNotice => {
            notes.push("grant_notice_without_active_session".into());
            Verdict::GrantWithoutSession
        }
        DeliveryObservation::RefusalOrMalformed => Verdict::Refused,
        DeliveryObservation::Unknown => {
            notes.push("delivery_unknown_exposure_retained".into());
            if active {
                // The gate reports active, but it is not attributable to this purchase:
                // reporting it as "granted" would claim evidence we do not have.
                notes.push("session_active_without_grant_notice".into());
            }
            Verdict::Unresolved
        }
    }
}

/// Baseline, send once, observe the session, and record the truth.
///
/// The order is the contract:
///
/// 1. **Ledger first.** If this operation already owns a send mark, this call is
///    status-only: nothing is sent, and the persisted delivery is what gets
///    classified. This check comes before the baseline so a replay of an
///    unresolved attempt is never mislabelled as "access already exists".
/// 2. **Baseline** (the preflight reachability check). Read the session. `active`
///    means the gate reports active: refuse before any send (zero POSTs).
///    Unreadable or unrecognised means no trustworthy baseline: refuse before any
///    send, because sending would commit the irreversible mark against a state
///    that cannot be compared afterwards.
/// 3. **Send once** through [`PolicyAdapter::attempt`] → `send_validated` →
///    [`ObservingTransport`]. A concurrent owner that appears between (1) and (3)
///    is discovered here, and is itself treated as a replay.
/// 4. **Observe** — bounded polling of the same endpoint, only when the gate may
///    have acted on us.
/// 5. **Publish** — one evidence record. No verdict reports established access.
pub fn purchase_once(
    adapter: &mut PolicyAdapter,
    request: &PurchaseRequest<'_>,
    sink: &EvidenceSink,
) -> Result<Published, PurchaseError> {
    let started_unix = unix_now();
    let started = Instant::now();
    let reference = parse_reference(&request.reference)?;
    if request.operation.is_empty() || request.operation.len() > 128 {
        return Err(PurchaseError::InvalidRequest("operation label"));
    }

    if request.pay_target.addr() != request.session_target.addr() {
        return Err(PurchaseError::InvalidRequest(
            "payment/session authority mismatch",
        ));
    }

    let vault = ProtectedTokenVault::open(&request.vault_path)
        .map_err(|e| PurchaseError::Vault(e.to_string()))?;
    let reader = SessionReader::new(
        request.session_target.clone(),
        request.context.clone(),
        request.limits,
    );
    let pay_digest = endpoint_digest(&request.pay_target);
    let session_digest = endpoint_digest(&request.session_target);

    let mut notes: Vec<String> = Vec::new();
    let mut baseline: Option<SessionState> = None;
    let mut baseline_error: Option<String> = None;
    let mut post_session: Option<SessionState> = None;
    let mut send = EvidenceSend {
        attempted: false,
        replayed: false,
        delivery: "not_sent".into(),
        notice: None,
        pre_send_refusal: false,
        outcome_detail: None,
        attempt_error: None,
    };
    let mut observation_not_durable = false;

    // ---- 1. what does the ledger already know? ----------------------------
    let mut owned = ledger_delivery(&request.ledger_path, &request.operation)?;
    if owned.is_some() {
        notes.push("operation_already_owned_status_only".into());
    }

    // ---- 2. baseline: preflight reachability + "do we already have access?" --
    match reader.read() {
        Ok(state) => {
            if state.state == SessionPhase::Unrecognized {
                notes.push("baseline_state_unrecognized".into());
            }
            baseline = Some(state);
        }
        Err(e) => baseline_error = Some(e.tag().to_string()),
    }

    let verdict: Verdict;
    match owned.take() {
        // ---- replay: observe the ledger's answer and the session, send nothing --
        Some(delivery) => {
            send.replayed = true;
            send.delivery = delivery_tag(delivery).into();
            notes.push("replay_status_only".into());
            notes.push("pre_send_refusal_unavailable_on_replay".into());
            let (state, error) = poll_session(&reader, request.poll);
            post_session = state;
            if let Some(error) = error {
                notes.push(format!("post_session_read_failed:{}", error.tag()));
            }
            if post_session
                .as_ref()
                .map(|s| s.state == SessionPhase::Unrecognized)
                .unwrap_or(false)
            {
                notes.push("post_session_unrecognized".into());
            }
            notes.push("replay_provenance_unbound".into());
            verdict = Verdict::Unresolved;
        }
        None => match (&baseline, &baseline_error) {
            (_, Some(_)) => {
                notes.push("no_trustworthy_baseline_no_send".into());
                verdict = Verdict::Unresolved;
            }
            (Some(state), None) if state.is_active() => {
                notes.push("baseline_active_no_send".into());
                verdict = Verdict::SessionAlreadyActive;
            }
            (Some(state), None) if state.state == SessionPhase::Unrecognized => {
                notes.push("baseline_unrecognized_no_send".into());
                verdict = Verdict::Unresolved;
            }
            (Some(_), None) => {
                // ---- 3. send once -------------------------------------------
                let mut transport = ObservingTransport::recording(
                    BoundVerifiedBytesTransport::new(
                        request.pay_target.clone(),
                        request.context.clone(),
                        request.limits,
                    ),
                    sink.clone(),
                    request.operation.clone(),
                    pay_digest.clone(),
                    (
                        request.context.interface().to_string(),
                        request.context.source(),
                    ),
                );
                let outcome = adapter.attempt(
                    request.enrollment,
                    &request.snapshot_bytes,
                    &vault,
                    &reference,
                    &request.operation,
                    request.day,
                    request.now,
                    &mut transport,
                );

                let forwarded = transport.sends() > 0;
                let record = transport.last().cloned();
                if record
                    .as_ref()
                    .map(|r| r.persist_error.is_some())
                    .unwrap_or(false)
                {
                    // The send happened; its record did not land. Report it, but
                    // keep the classification the transport observed.
                    notes.push("observation_record_not_durable".into());
                    observation_not_durable = true;
                }
                send.attempted = forwarded;

                // A concurrent owner can win between the probe above and the
                // ledger's own transaction. If so, this call is a replay: the
                // persisted answer is the authority, and no body was forwarded.
                let mut replay_delivery: Option<DeliveryObservation> = None;
                if !forwarded {
                    if let Some(delivery) =
                        ledger_delivery(&request.ledger_path, &request.operation)?
                    {
                        replay_delivery = Some(delivery);
                    }
                }

                match (outcome, replay_delivery) {
                    (Err(e), None) => {
                        send.attempt_error = Some(e);
                        notes.push("attempt_refused_before_transport".into());
                        verdict = Verdict::Refused;
                    }
                    (outcome, replay) => {
                        // On a replay the transport never ran, so the ledger is
                        // the authority for what was observed.
                        let delivery = match (&record, replay) {
                            (Some(record), _) => {
                                delivery_from_tag(&record.delivery).ok_or_else(|| {
                                    PurchaseError::Evidence("unknown delivery tag".into())
                                })?
                            }
                            (None, Some(delivery)) => delivery,
                            (None, None) => {
                                return Err(PurchaseError::Ledger(
                                    "transport did not run and no send is owned".into(),
                                ))
                            }
                        };
                        if let Some(record) = &record {
                            send.notice = record.notice.clone();
                            send.pre_send_refusal = record.pre_send_refusal;
                            send.outcome_detail = record.detail.clone();
                        }
                        let replaying = replay.is_some() || !forwarded;
                        send.replayed = replaying;
                        if replaying {
                            notes.push("replay_status_only".into());
                            if record.is_none() {
                                notes.push("pre_send_refusal_unavailable_on_replay".into());
                            }
                            if let Err(e) = outcome {
                                send.attempt_error = Some(e);
                            }
                        }
                        send.delivery = delivery_tag(delivery).into();
                        if send.pre_send_refusal {
                            notes.push("pre_send_refusal_keeps_exposure".into());
                            notes.push("manual_reconciliation_required".into());
                        }

                        // ---- 4. observe --------------------------------------
                        // Poll whenever a byte may have reached the gate. An
                        // ambiguous write is exactly the case where the session
                        // could have opened silently, so it must be polled; only
                        // a proven pre-send refusal (no byte written, not a
                        // replay) makes the question meaningless.
                        if !send.pre_send_refusal || replaying {
                            let (state, error) = poll_session(&reader, request.poll);
                            post_session = state;
                            if let Some(error) = error {
                                notes.push(format!("post_session_read_failed:{}", error.tag()));
                            }
                            if post_session
                                .as_ref()
                                .map(|s| s.state == SessionPhase::Unrecognized)
                                .unwrap_or(false)
                            {
                                notes.push("post_session_unrecognized".into());
                            }
                        }
                        let active = post_session
                            .as_ref()
                            .map(|s| s.is_active())
                            .unwrap_or(false);
                        let context_matches = match (&baseline, &post_session, &send.notice) {
                            (Some(before), Some(after), Some(notice)) => {
                                before.mac == after.mac
                                    && notice
                                        .device_mac
                                        .as_ref()
                                        .map(|mac| Some(mac) == after.mac.as_ref())
                                        .unwrap_or(true)
                                    && after
                                        .metric
                                        .as_ref()
                                        .map(|metric| metric == &notice.metric)
                                        .unwrap_or(true)
                            }
                            _ => true,
                        };
                        verdict = if replaying {
                            notes.push("replay_provenance_unbound".into());
                            Verdict::Unresolved
                        } else if !context_matches {
                            notes.push("session_context_mismatch".into());
                            Verdict::Unresolved
                        } else {
                            notes.push("purchase_access_attribution_unavailable".into());
                            judge(delivery, active, &mut notes)
                        };
                    }
                }
            }
            (None, None) => {
                // Unreachable: a read yields either a state or an error.
                notes.push("baseline_absent".into());
                verdict = Verdict::Unresolved;
            }
        },
    }

    let evidence = Evidence {
        schema: EVIDENCE_SCHEMA.into(),
        operation: request.operation.clone(),
        offer_digest: request.offer_digest.clone(),
        payment_endpoint_digest: pay_digest,
        session_endpoint_digest: session_digest,
        network: EvidenceNetwork {
            interface: request.context.interface().to_string(),
            source: request.context.source().to_string(),
        },
        baseline: baseline.as_ref().map(session_evidence),
        baseline_error,
        send,
        post_session: post_session.as_ref().map(session_evidence),
        timestamps: EvidenceTimestamps {
            started_unix,
            finished_unix: unix_now(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        verdict,
        notes,
    };

    let encoded =
        serde_json::to_vec_pretty(&evidence).map_err(|e| PurchaseError::Evidence(e.to_string()))?;
    let path = sink
        .write("evidence", &encoded)
        .map_err(|e| PurchaseError::Evidence(e.to_string()))?;

    if observation_not_durable {
        // The evidence file itself is on disk, so the caller can still see what
        // happened; but the per-operation observation is missing, and that is an
        // error for anything that expects to reconcile later.
        return Err(PurchaseError::Evidence(
            "per-operation observation record was not durable".into(),
        ));
    }
    Ok(Published { path, evidence })
}

// ------------------------------------------------------------------ tests ----

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(status: u16, body: &'static [u8]) -> SendOutcome {
        SendOutcome::Complete {
            status,
            body: body.to_vec(),
        }
    }

    #[test]
    fn a_grant_notice_requires_both_tags_and_kind_1022() {
        let grant = br#"{"kind":1022,"tags":[["allotment","60000"],["metric","milliseconds"]]}"#;
        let parsed = parse_send_outcome((&complete(200, grant)).into());
        assert_eq!(parsed.delivery, DeliveryObservation::GrantNotice);
        let notice = parsed.notice.unwrap();
        assert_eq!(notice.allotment, 60_000);
        assert_eq!(notice.metric, "milliseconds");
        assert_eq!(notice.raw_digest.len(), 64);

        let rejected: Vec<&'static [u8]> = vec![
            br#"{"kind":1022,"tags":[["metric","milliseconds"]]}"#,
            br#"{"kind":1022,"tags":[["allotment","60000"]]}"#,
            br#"{"kind":1022,"tags":[["allotment","1"],["allotment","2"],["metric","b"]]}"#,
            br#"{"kind":1022,"tags":[["allotment","1"],["metric","b"],["metric","c"]]}"#,
            br#"{"kind":1022,"tags":[["allotment","1"],["metric",""]]}"#,
            br#"{"kind":1022,"tags":[["allotment","0"],["metric","b"]]}"#,
            br#"{"kind":1022,"tags":[["allotment",1],["metric","b"]]}"#,
            br#"{"kind":1022}"#,
            br#"{"kind":21023,"tags":[["allotment","1"],["metric","b"]]}"#,
            br#"{"kind":1022,"tags":[["allotment","1"],["metric","b"],["x"]]}"#,
            b"not json at all",
            b"",
        ];
        for body in rejected {
            let parsed = parse_send_outcome((&complete(200, body)).into());
            assert_eq!(
                parsed.delivery,
                DeliveryObservation::RefusalOrMalformed,
                "{}",
                String::from_utf8_lossy(body)
            );
            assert!(parsed.notice.is_none());
            assert!(parsed.detail.is_some());
        }
    }

    #[test]
    fn a_complete_non_2xx_answer_is_malformed_even_with_a_grant_body() {
        let grant = br#"{"kind":1022,"tags":[["allotment","1"],["metric","b"]]}"#;
        let parsed = parse_send_outcome((&complete(400, grant)).into());
        assert_eq!(parsed.delivery, DeliveryObservation::RefusalOrMalformed);
        assert!(parsed.detail.unwrap().starts_with("unexpected_status:400"));
    }

    #[test]
    fn ambiguity_is_unknown_and_a_pre_send_refusal_is_unknown_but_flagged() {
        let ambiguous = SendOutcome::Ambiguous {
            reason: AmbiguousReason::Truncated,
        };
        let parsed = parse_send_outcome((&ambiguous).into());
        assert_eq!(parsed.delivery, DeliveryObservation::Unknown);
        assert!(!parsed.pre_send_refusal);
        assert!(parsed.detail.unwrap().starts_with("ambiguous:"));

        let refused = SendOutcome::RefusedBeforeSend {
            reason: RefusalReason::SourceBindFailed("EADDRNOTAVAIL".into()),
        };
        let parsed = parse_send_outcome((&refused).into());
        assert_eq!(
            parsed.delivery,
            DeliveryObservation::Unknown,
            "the irreversible send mark is already committed before transport runs"
        );
        assert!(
            parsed.pre_send_refusal,
            "the distinction survives for the reconciler"
        );
        assert!(parsed.detail.unwrap().starts_with("refused_before_send:"));
    }

    #[test]
    fn safety_session_metric_and_device_are_validated() {
        assert!(parse_session_body(br#"{"state":"active","metric":"cashuA_reflected"}"#).is_err());
        assert!(parse_session_body(br#"{"state":"active","mac":"mac"}"#).is_err());
    }

    #[test]
    fn session_phase_never_guesses() {
        assert_eq!(SessionPhase::from_wire("none"), SessionPhase::Inactive);
        assert_eq!(SessionPhase::from_wire("active"), SessionPhase::Active);
        assert_eq!(
            SessionPhase::from_wire("Active"),
            SessionPhase::Unrecognized,
            "the wire vocabulary is exact"
        );
        assert_eq!(
            SessionPhase::from_wire("none "),
            SessionPhase::Unrecognized,
            "no trimming"
        );
        assert_eq!(SessionPhase::from_wire(""), SessionPhase::Unrecognized);
        assert_eq!(
            parse_session_body(br#"{"status":1}"#),
            Err(SessionReadError::Malformed("no state"))
        );
        assert_eq!(
            parse_session_body(b""),
            Err(SessionReadError::Malformed("empty body"))
        );
        assert_eq!(
            parse_session_body(b"[]"),
            Err(SessionReadError::Malformed("not an object"))
        );
        assert_eq!(
            parse_session_body(br#"{"state":"active","allotment":0}"#),
            Err(SessionReadError::Malformed("bad allotment"))
        );
        assert_eq!(
            parse_session_body(br#"{"state":"active","metric":""}"#),
            Err(SessionReadError::Malformed("bad metric"))
        );

        let (phase, allotment, metric) =
            parse_session_body(br#"{"state":"active","allotment":"60000","metric":"bytes"}"#)
                .unwrap();
        assert_eq!(phase, SessionPhase::Active);
        assert_eq!(allotment, Some(60_000));
        assert_eq!(metric.as_deref(), Some("bytes"));

        let (phase, allotment, metric) = parse_session_body(br#"{"state":"none"}"#).unwrap();
        assert_eq!(phase, SessionPhase::Inactive);
        assert_eq!(allotment, None);
        assert_eq!(metric, None);

        // A bare number is accepted for allotment (non-tag payloads), because the
        // value is unambiguous; a zero is not.
        let (_, allotment, _) =
            parse_session_body(br#"{"state":"active","allotment":60000}"#).unwrap();
        assert_eq!(allotment, Some(60_000));
    }

    #[test]
    fn no_verdict_reports_access() {
        for verdict in [
            Verdict::GrantWithoutSession,
            Verdict::SessionAlreadyActive,
            Verdict::Refused,
            Verdict::Unresolved,
        ] {
            assert!(!verdict.reports_access(), "{verdict:?}");
        }
        assert!(!Verdict::GrantAndSessionActive.reports_access());
        assert_eq!(
            serde_json::to_string(&Verdict::GrantWithoutSession).unwrap(),
            "\"grant_without_session\""
        );
        let mut notes = Vec::new();
        assert_eq!(
            judge(DeliveryObservation::GrantNotice, false, &mut notes),
            Verdict::GrantWithoutSession
        );
        assert_eq!(
            judge(DeliveryObservation::Unknown, true, &mut notes),
            Verdict::Unresolved,
            "an active session we cannot attribute is not a grant"
        );
        assert!(notes
            .iter()
            .any(|n| n == "session_active_without_grant_notice"));
    }

    #[test]
    fn an_evidence_sink_refuses_a_non_private_directory() {
        let dir = PathBuf::from("/dev/shm").join(format!(
            "capped-evidence-{}-{}",
            std::process::id(),
            unix_now()
        ));
        let sink = EvidenceSink::open(&dir).unwrap();
        assert!(sink.path().is_dir());
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        assert!(EvidenceSink::open(&dir).is_err(), "0755 must be refused");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            EvidenceSink::open("relative/evidence").is_err(),
            "a relative evidence directory is refused"
        );
    }
}
