//! The injectable socket-binding seam: one place where a gate socket is attached
//! to the chosen network, shared by every request path.
//!
//! Four request shapes talk to a gate — the advert `GET`, the session-state
//! `GET`, the payment `POST`, and the session probe after a grant. Each one used
//! to build its own socket, and the Android shell had no way to say *which*
//! network the socket must leave from: it selected a `Network`, dropped the
//! object, and the kernel was free to route the request over cellular while the
//! phone sat on captive Wi-Fi.
//!
//! [`NetworkBinder`] is that missing statement. A [`NetworkSelection`] carries
//! the caller's [`NetworkContext`] (source address + interface label) and a
//! binder; [`connect_bound`] creates the socket, asks the binder to attach it,
//! source-binds, connects, and then verifies the two endpoints that the kernel
//! actually produced. Nothing else in the crate creates a gate socket.
//!
//! Two platform binders ship here:
//!
//! * [`DeviceBinder`] — Linux `SO_BINDTODEVICE` on the context's interface,
//!   exactly the behaviour the shipped daemon already had.
//! * [`AndroidNetworkBinder`] — `android_setsocknetwork(handle, fd)` from
//!   libandroid, with a [`NetworkLink`] the shell updates when the network goes
//!   away. **Not wired to Java yet**: no `Network.getNetworkHandle()` is captured
//!   in this change, and until one is, the Android default
//!   ([`NetworkSelection::default_for`] on `target_os = "android"`][no-network])
//!   is [`NoNetwork`] and refuses every request instead of falling back to an
//!   unbound socket.
//!
//! [no-network]: NetworkSelection::default_for
//!
//! Rules that are structural here rather than policy:
//!
//! * no process-wide binding — every call binds one socket (`android_setprocnetwork`
//!   is never called),
//! * no unbound fallback — a binder that cannot attach the socket is an error,
//! * no retry and no redirect — a single connect, a single write,
//! * loss of the selected network *before* the write refuses the request; loss
//!   discovered *after* a possible write is ambiguous, never "not sent".

use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

use crate::bound_transport::{
    read_response, AmbiguousReason, BoundTarget, Limits, NetworkContext, RefusalReason,
};

// --------------------------------------------------------------- the binder ---

/// A network that a socket can be attached to, and whose liveness can be
/// rechecked at the send boundary.
///
/// The methods are deliberately small: `bind` is the platform call, `link_intact`
/// is the "is this still the network we chose?" question, and `kind` is a stable
/// label for evidence (never a credential, never user text).
pub trait NetworkBinder: Send + Sync {
    /// Attach a freshly created, not-yet-connected socket to this network.
    fn bind(&self, socket: &Socket) -> Result<(), BindError>;

    /// Is the selected network and its link generation still the one the caller
    /// chose? Called immediately before the write and once more after it.
    fn link_intact(&self) -> bool;

    /// Monotonic identity of the current network generation. `0` means the
    /// platform exposes no generation (the Linux device path).
    fn generation(&self) -> u64;

    /// Stable label for evidence and typed refusals.
    fn kind(&self) -> &'static str;

    /// How a loss of this network is named. The Linux device path keeps its
    /// historical interface name; a platform handle reports a generation loss,
    /// because a reused interface name or address is not the same network.
    fn lost(&self) -> LostNetwork;
}

/// What went wrong while attaching/connecting a socket, in terms a caller can
/// map onto its own typed outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// `SO_BINDTODEVICE` refused the interface.
    Interface {
        interface: String,
        detail: String,
    },
    /// A platform network handle could not be attached to the socket.
    Network {
        kind: &'static str,
        detail: String,
    },
    /// The selected network/link is gone at a boundary.
    Lost(LostNetwork),
    SourceBind(String),
    ConnectTimeout,
    Connect(String),
    /// The kernel put the socket on a different local address than requested.
    SourceMismatch {
        expected: IpAddr,
        actual: IpAddr,
    },
    /// The kernel connected it to a different peer than requested.
    PeerMismatch {
        expected: SocketAddr,
        actual: SocketAddr,
    },
}

/// How a lost network is named. The Linux device path keeps its historical
/// `InterfaceVanished`; a platform handle reports a generation loss, because a
/// reused interface name or address is not the same network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LostNetwork {
    Interface(String),
    Generation { kind: &'static str, generation: u64 },
}

/// Linux: `SO_BINDTODEVICE`, unchanged from the reviewed send path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceBinder {
    interface: String,
}

impl DeviceBinder {
    pub fn new(interface: impl Into<String>) -> Self {
        Self {
            interface: interface.into(),
        }
    }
}

impl NetworkBinder for DeviceBinder {
    fn bind(&self, socket: &Socket) -> Result<(), BindError> {
        socket
            .bind_device(Some(self.interface.as_bytes()))
            .map_err(|e| BindError::Interface {
                interface: self.interface.clone(),
                detail: e.to_string(),
            })
    }

    fn link_intact(&self) -> bool {
        std::path::Path::new(&format!("/sys/class/net/{}", self.interface)).exists()
    }

    fn generation(&self) -> u64 {
        0
    }

    fn kind(&self) -> &'static str {
        "bindtodevice"
    }

    fn lost(&self) -> LostNetwork {
        LostNetwork::Interface(self.interface.clone())
    }
}

/// Liveness of one Android `Network`, shared between the Java callback that owns
/// it and the Rust sockets bound to it.
///
/// The shell creates one per selected network, records the generation
/// (`Network` identity as seen by `ConnectivityManager`), and calls
/// [`NetworkLink::mark_lost`] from `onLost`/`onUnavailable`. **This half is not
/// implemented in Java yet**; the type exists so the JS-side follow-up card has a
/// defined contract, and so the recheck can be exercised on the host.
#[derive(Debug)]
pub struct NetworkLink {
    generation: AtomicU64,
    lost: AtomicBool,
}

impl NetworkLink {
    pub fn new(generation: u64) -> Arc<Self> {
        Arc::new(Self {
            generation: AtomicU64::new(generation),
            lost: AtomicBool::new(false),
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Irreversible: a lost network is never resurrected. A replacement network
    /// is a different generation and a different selection.
    pub fn mark_lost(&self) {
        self.lost.store(true, Ordering::SeqCst);
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }
}

/// Android: `android_setsocknetwork(Network.getNetworkHandle(), fd)`.
///
/// On a non-Android build the symbol does not exist, so `bind` fails closed
/// instead of quietly producing an unbound socket. That is a contract worth
/// testing on the host: the seam must never degrade.
#[derive(Clone)]
pub struct AndroidNetworkBinder {
    handle: u64,
    link: Arc<NetworkLink>,
}

impl std::fmt::Debug for AndroidNetworkBinder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AndroidNetworkBinder")
            .field("handle_present", &(self.handle != 0))
            .field("generation", &self.link.generation())
            .field("lost", &self.link.is_lost())
            .finish()
    }
}

#[cfg(target_os = "android")]
mod android_ffi {
    use std::os::unix::io::AsRawFd;

    // libandroid, API 23+. The handle is `Network.getNetworkHandle()` widened to
    // u64; zero is NETWORK_UNSPECIFIED and is never passed.
    #[link(name = "android")]
    extern "C" {
        fn android_setsocknetwork(network: u64, fd: std::os::raw::c_int) -> std::os::raw::c_int;
    }

    pub(super) fn attach(handle: u64, socket: &socket2::Socket) -> Result<(), String> {
        // SAFETY: the socket outlives the call and the fd is owned by it.
        let rc = unsafe { android_setsocknetwork(handle, socket.as_raw_fd()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }
}

#[cfg(target_os = "android")]
fn attach_android(handle: u64, socket: &Socket) -> Result<(), String> {
    android_ffi::attach(handle, socket)
}

#[cfg(not(target_os = "android"))]
fn attach_android(_handle: u64, _socket: &Socket) -> Result<(), String> {
    Err("android_setsocknetwork is not available on this build target".into())
}

impl AndroidNetworkBinder {
    /// `handle` must be a real `Network.getNetworkHandle()`: zero is
    /// `NETWORK_UNSPECIFIED` and would clear a binding, so it is refused.
    pub fn new(handle: u64, link: Arc<NetworkLink>) -> Result<Self, BindError> {
        if handle == 0 {
            return Err(BindError::Network {
                kind: "android-setsocknetwork",
                detail: "NETWORK_UNSPECIFIED is not a network".into(),
            });
        }
        Ok(Self { handle, link })
    }
}

impl NetworkBinder for AndroidNetworkBinder {
    fn bind(&self, socket: &Socket) -> Result<(), BindError> {
        attach_android(self.handle, socket).map_err(|detail| BindError::Network {
            kind: "android-setsocknetwork",
            detail,
        })?;
        // The platform call is not a promise that the network is still alive.
        if self.link.is_lost() {
            return Err(BindError::Lost(self.lost()));
        }
        Ok(())
    }

    fn link_intact(&self) -> bool {
        !self.link.is_lost()
    }

    fn generation(&self) -> u64 {
        self.link.generation()
    }

    fn kind(&self) -> &'static str {
        "android-setsocknetwork"
    }

    fn lost(&self) -> LostNetwork {
        LostNetwork::Generation {
            kind: "android-setsocknetwork",
            generation: self.link.generation(),
        }
    }
}

/// Fail-closed placeholder: no platform network has been injected yet.
///
/// This is what an Android build uses until a follow-up card feeds it a real
/// handle. It refuses every bind, so the failure is "no payment traffic" rather
/// than "payment traffic on an unspecified network".
#[derive(Debug, Clone, Copy)]
pub struct NoNetwork {
    detail: &'static str,
}

impl NoNetwork {
    pub const fn new(detail: &'static str) -> Self {
        Self { detail }
    }
}

impl NetworkBinder for NoNetwork {
    fn bind(&self, _socket: &Socket) -> Result<(), BindError> {
        Err(BindError::Network {
            kind: "no-network",
            detail: self.detail.to_string(),
        })
    }

    fn link_intact(&self) -> bool {
        false
    }

    fn generation(&self) -> u64 {
        0
    }

    fn kind(&self) -> &'static str {
        "no-network"
    }

    fn lost(&self) -> LostNetwork {
        LostNetwork::Generation {
            kind: "no-network",
            generation: 0,
        }
    }
}

// ------------------------------------------------------------- the selection ---

/// One caller's chosen network: which local address, which interface label, and
/// which binder attaches sockets to it.
#[derive(Clone)]
pub struct NetworkSelection {
    context: NetworkContext,
    binder: Arc<dyn NetworkBinder>,
}

impl std::fmt::Debug for NetworkSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkSelection")
            .field("interface", &self.context.interface())
            .field("source", &self.context.source())
            .field("binder", &self.binder.kind())
            .finish()
    }
}

impl NetworkSelection {
    /// Inject a binder explicitly. Tests use this; the Android shell will use it
    /// once it has a network handle.
    pub fn with(context: NetworkContext, binder: Arc<dyn NetworkBinder>) -> Self {
        Self { context, binder }
    }

    /// The platform default for a context.
    ///
    /// Linux: `SO_BINDTODEVICE` on the context interface — the reviewed
    /// behaviour, unchanged. Android: [`NoNetwork`], i.e. fail closed, because a
    /// socket on an Android build must not be created without a specific
    /// `Network` handle.
    pub fn default_for(context: NetworkContext) -> Self {
        let binder = default_binder(context.interface());
        Self { context, binder }
    }

    /// The Android seam: attach every socket to this `Network` handle.
    pub fn android_network(
        context: NetworkContext,
        handle: u64,
        link: Arc<NetworkLink>,
    ) -> Result<Self, BindError> {
        Ok(Self {
            context,
            binder: Arc::new(AndroidNetworkBinder::new(handle, link)?),
        })
    }

    pub fn context(&self) -> &NetworkContext {
        &self.context
    }

    pub fn binder(&self) -> &Arc<dyn NetworkBinder> {
        &self.binder
    }
}

#[cfg(not(target_os = "android"))]
fn default_binder(interface: &str) -> Arc<dyn NetworkBinder> {
    Arc::new(DeviceBinder::new(interface))
}

#[cfg(target_os = "android")]
fn default_binder(_interface: &str) -> Arc<dyn NetworkBinder> {
    // Nothing in this crate captures a Network yet. Refusing is the only honest
    // answer; an unbound socket here is exactly the bug this seam exists to stop.
    Arc::new(NoNetwork::new(
        "no Android Network handle has been injected for this operation",
    ))
}

// -------------------------------------------------------------- the socket ----

/// A connected socket that is on the selected network, with the endpoint
/// verification and the generation recheck kept next to the connection it
/// describes.
pub struct BoundSocket {
    stream: TcpStream,
}

impl BoundSocket {
    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    pub fn into_stream(self) -> TcpStream {
        self.stream
    }

    /// Recheck the selection. Called immediately before the write and once more
    /// after it; the caller decides what a loss means at each point.
    pub fn recheck(&self, binder: &dyn NetworkBinder) -> Result<(), BindError> {
        if binder.link_intact() {
            Ok(())
        } else {
            Err(BindError::Lost(binder.lost()))
        }
    }
}

/// Create, attach, source-bind, connect and verify one socket.
///
/// The order is the contract: the platform attach happens **before** the source
/// bind (an Android network handle and a `SO_BINDTODEVICE` are both applied to an
/// unconnected socket), and both endpoints are verified afterwards so a silently
/// re-routed socket is a refusal rather than a warning.
pub fn connect_bound(
    target: SocketAddr,
    context: &NetworkContext,
    binder: &dyn NetworkBinder,
    connect_timeout: Duration,
) -> Result<BoundSocket, BindError> {
    let domain = if target.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(|e| BindError::Connect(e.to_string()))?;
    let _ = socket.set_tcp_nodelay(true);

    // No unbound fallback: a binder that cannot attach the socket is an error.
    binder.bind(&socket)?;
    socket
        .bind(&SocketAddr::new(context.source(), 0).into())
        .map_err(|e| BindError::SourceBind(e.to_string()))?;

    match socket.connect_timeout(&target.into(), connect_timeout) {
        Ok(()) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::TimedOut
                || e.kind() == std::io::ErrorKind::WouldBlock =>
        {
            return Err(BindError::ConnectTimeout)
        }
        Err(e) => return Err(BindError::Connect(e.to_string())),
    }

    let stream: TcpStream = socket.into();
    match stream.local_addr() {
        Ok(local) if local.ip() == context.source() => {}
        Ok(local) => {
            return Err(BindError::SourceMismatch {
                expected: context.source(),
                actual: local.ip(),
            })
        }
        Err(e) => return Err(BindError::SourceBind(e.to_string())),
    }
    match stream.peer_addr() {
        Ok(peer) if peer == target => {}
        Ok(peer) => {
            return Err(BindError::PeerMismatch {
                expected: target,
                actual: peer,
            })
        }
        Err(e) => return Err(BindError::Connect(e.to_string())),
    }
    Ok(BoundSocket { stream })
}

/// Pre-write network recheck shared by the POST and the GET paths.
pub fn ensure_link(socket: &BoundSocket, binder: &dyn NetworkBinder) -> Result<(), BindError> {
    socket.recheck(binder)
}

// ---------------------------------------------------------------- bound GET ---

/// One bounded `GET` through the seam: the advert read and the session probe both
/// use this, so a probe cannot accidentally be less bound than the payment.
#[derive(Debug, Clone)]
pub struct BoundGet {
    selection: NetworkSelection,
    limits: Limits,
}

/// A complete answer: a 2xx status and a body read under strict framing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetAnswer {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetError {
    /// Refused before any request byte was written.
    RefusedBeforeSend(RefusalReason),
    /// The request may have been delivered; the answer is not usable.
    Ambiguous(AmbiguousReason),
}

impl GetError {
    pub fn may_have_been_sent(&self) -> bool {
        matches!(self, GetError::Ambiguous(_))
    }
}

impl BoundGet {
    pub fn new(selection: NetworkSelection, limits: Limits) -> Self {
        Self { selection, limits }
    }

    pub fn selection(&self) -> &NetworkSelection {
        &self.selection
    }

    /// One `GET`, one response, one typed answer. Never retried, never redirected.
    pub fn read(&self, target: &BoundTarget) -> Result<GetAnswer, GetError> {
        let context = &self.selection.context;
        if context.interface().is_empty() {
            return Err(GetError::RefusedBeforeSend(RefusalReason::InvalidContext(
                "interface absent",
            )));
        }
        if context.source().is_unspecified() {
            return Err(GetError::RefusedBeforeSend(RefusalReason::InvalidContext(
                "source address absent",
            )));
        }
        if context.source().is_ipv4() != target.addr().is_ipv4() {
            return Err(GetError::RefusedBeforeSend(
                RefusalReason::AddressFamilyMismatch {
                    target: target.addr().ip(),
                    source: context.source(),
                },
            ));
        }

        let deadline = Instant::now() + self.limits.total;
        let binder = self.selection.binder.as_ref();
        let socket = connect_bound(target.addr(), context, binder, self.limits.connect)
            .map_err(|e| GetError::RefusedBeforeSend(refusal(e, context)))?;

        // ---- pre-write generation recheck: refuse, do not send on a dead link --
        ensure_link(&socket, binder)
            .map_err(|e| GetError::RefusedBeforeSend(refusal(e, context)))?;

        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             User-Agent: tollgate-client-core/bound-get\r\n\
             Accept: */*\r\n\
             Accept-Encoding: identity\r\n\
             Connection: close\r\n\
             \r\n",
            path = target.path(),
            host = target.addr(),
        );

        let stream = socket.stream();
        if let Err(e) = stream.set_write_timeout(Some(self.limits.write)) {
            return Err(GetError::RefusedBeforeSend(RefusalReason::ConnectFailed(
                e.to_string(),
            )));
        }
        if let Err(e) = stream.set_read_timeout(Some(self.limits.read)) {
            return Err(GetError::RefusedBeforeSend(RefusalReason::ConnectFailed(
                e.to_string(),
            )));
        }
        if Instant::now() >= deadline {
            return Err(GetError::RefusedBeforeSend(RefusalReason::ConnectTimeout));
        }

        // ---- irreversible boundary: exactly one write, no retry ----
        {
            use std::io::Write;
            if let Err(e) = (&mut &*stream).write_all(request.as_bytes()) {
                return Err(GetError::Ambiguous(write_ambiguity(e)));
            }
        }

        // ---- loss after a possible write is ambiguous, never "not sent" ----
        socket
            .recheck(binder)
            .map_err(|e| GetError::Ambiguous(ambiguous(e, context)))?;

        read_response(stream, self.limits, deadline)
            .map(|(status, body)| GetAnswer { status, body })
            .map_err(GetError::Ambiguous)
    }
}

fn write_ambiguity(error: std::io::Error) -> AmbiguousReason {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            AmbiguousReason::WriteTimeout
        }
        _ => AmbiguousReason::WriteFailed(error.to_string()),
    }
}

/// `BindError` → the POST path's refusal vocabulary. The Linux device bind keeps
/// its historical `InterfaceBindingFailed`/`InterfaceVanished` variants.
pub(crate) fn refusal(error: BindError, context: &NetworkContext) -> RefusalReason {
    match error {
        BindError::Interface { detail, .. } => RefusalReason::InterfaceBindingFailed(detail),
        BindError::Network { kind, detail } => RefusalReason::NetworkBindingFailed { kind, detail },
        BindError::Lost(LostNetwork::Interface(_)) => {
            RefusalReason::InterfaceVanished(context.interface().to_string())
        }
        BindError::Lost(LostNetwork::Generation { kind, generation }) => {
            RefusalReason::NetworkGenerationLost { kind, generation }
        }
        BindError::SourceBind(detail) => RefusalReason::SourceBindFailed(detail),
        BindError::ConnectTimeout => RefusalReason::ConnectTimeout,
        BindError::Connect(detail) => RefusalReason::ConnectFailed(detail),
        BindError::SourceMismatch { expected, actual } => {
            RefusalReason::SourceAddressMismatch { expected, actual }
        }
        BindError::PeerMismatch { expected, actual } => {
            RefusalReason::PeerAddressMismatch { expected, actual }
        }
    }
}

/// `BindError` → ambiguity for a boundary that has already written bytes.
pub(crate) fn ambiguous(error: BindError, context: &NetworkContext) -> AmbiguousReason {
    match error {
        BindError::Lost(LostNetwork::Interface(_)) => {
            AmbiguousReason::NetworkInterfaceLost(context.interface().to_string())
        }
        BindError::Lost(LostNetwork::Generation { kind, generation }) => {
            AmbiguousReason::NetworkGenerationLost { kind, generation }
        }
        // Anything else at a post-write boundary is a plain write/read failure.
        other => AmbiguousReason::WriteFailed(format!("{other:?}")),
    }
}
