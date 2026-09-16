//! Unified multi-service host for peerline.
//!
//! Every peerline service is just "a function that registers handlers on a
//! [`Peer`]". This crate turns that into a composable unit — the
//! [`Service`] trait — and a [`Host`] that mounts many services behind one
//! transport stack, so a fleet of services runs from **one binary** instead
//! of one binary each.
//!
//! Each service is **mounted at an address you configure** ([`Mount`]), per
//! transport, so a connection reaches exactly one service and their ops
//! never collide:
//!
//! - WebSocket — a path on the shared port (`/` for a lone service, `/foo`
//!   when several share a port).
//! - Unix socket — a socket file (the filesystem is the namespace).
//! - iroh — an ALPN on the shared endpoint (one ticket, dialer picks the
//!   ALPN).
//!
//! Addresses are explicit rather than derived from the name, so migrating a
//! single-service daemon keeps its exact wire addresses (WS `/`, its socket
//! path), while a multi-service host gives each a distinct mount.
//!
//! ```ignore
//! let mut host = Host::new().mount(
//!     Sh::new(state),
//!     Mount::new()
//!         .ws("/", WsPolicy::origins(["https://app.example"]))
//!         .uds(cfg.socket), // admits the same user (and root) by default
//! );
//! if let Some(addr) = cfg.ws_bind { host = host.ws_bind(addr); }
//! host.run().await?;   // binds transports, serves until SIGINT / SIGTERM
//! ```
//!
//! The host owns the boilerplate every service used to hand-roll: binding
//! each transport, requiring at least one active mount, iroh key
//! persistence + ticket emission, and graceful shutdown.
//!
//! # Learning your own address: [`Host::bind`]
//!
//! [`run`](Host::run) binds and serves in one step, which is what almost every
//! service wants. A service that has to *know* what it bound — because it
//! asked for port `0` and has to report the port the kernel picked, or because
//! it hands its iroh ticket to somebody — splits the two:
//!
//! ```ignore
//! let Bound::Listening(bound) = host.bind().await? else { return Ok(()) };
//! if let Some(addr) = bound.ws_addr() {
//!     hello.publish(format!("ws://{addr}/"));     // the resolved port
//! }
//! bound.serve_until(shutdown).await?;
//! ```
//!
//! The listeners are open across that gap, so a client that dials while the
//! caller is still deciding queues in the kernel backlog rather than being
//! refused. `run` and `run_until` are thin wrappers over exactly this pair.
//!
//! # Failures are types, not sentences
//!
//! Every fallible entry point answers with [`HostError`], [`PlanError`],
//! [`BindError`] or [`DialError`] rather than a `String`, so a supervisor can
//! tell a misconfiguration (which will fail identically forever) from a busy
//! port (which may not), and a service can tell a producer that **refused**
//! it from one that is merely down. Each converts into a `String`, so code
//! written against the old signature keeps compiling.
//!
//! # Joining a cluster
//!
//! A service that needs *another* service declares a **contract** rather than
//! an address ([`Service::consumes`]), and a host that is given a coordinator
//! resolves it at startup:
//!
//! ```ignore
//! host.coordinator(peerline_contract::coord::DEFAULT_SOCKET)
//!     .run()
//!     .await?;
//! ```
//!
//! ```text
//! Bind -> Register -> AwaitReady -> Service::bind(deps) -> Serve -> Ready
//!             |
//!   heartbeat starts here, concurrent with everything to its right
//! ```
//!
//! Binding first is what makes the registration honest: an ephemeral ws port
//! and an iroh ticket are not known until the socket exists. Serving only
//! after [`Service::bind`] is what makes the dependency handles safe: no
//! handler can run before its dependencies are in hand. And because `bind`
//! and `accept` are separate syscalls, a client that dials during the
//! resolving window queues in the kernel backlog instead of being refused.
//!
//! **The coordinator is opt-in.** A host that never calls
//! [`Host::coordinator`] makes no outbound connection and never waits for
//! anything — which is how the coordinator's own host avoids registering with
//! itself. A service that declares `consumes` without one is a configuration
//! error and fails at [`Host::run`] rather than hanging.
//!
//! # Reporting a manifest instead of serving
//!
//! With `PEERLINE_MANIFEST_OUT=/path/to.json` in the environment,
//! [`Host::run`] writes what its services produce and consume to that path and
//! exits 0 without binding anything. That is how a supervisor learns the
//! contract graph: from the binary, which is the only thing that cannot drift
//! from the code. See [`peerline_contract::manifest`].
//!
//! # A host cannot outlive its supervisor
//!
//! A supervisor that spawns a host may hand it a **lifeline**: an inherited
//! pipe, named in `PEERLINE_LIFELINE_FD`, whose write end only the supervisor
//! holds. [`Host::run_until`] watches it, and EOF — which the kernel produces
//! when the last write end closes, whatever killed the supervisor — takes the
//! same shutdown path as `SIGTERM`. That is a *structural* guarantee rather
//! than a cooperative one: a supervisor cannot skip it by dying in a way that
//! skips its own cleanup, which is exactly how orphaned clusters happen. See
//! [`peerline_contract::lifeline`].
//!
//! **No variable, no lifeline, no difference.** A service run by hand, by
//! systemd or by a test behaves precisely as it always has.
//!
//! # `unsafe`
//!
//! One module, `lifeline` (private), adopts a file descriptor the kernel handed this
//! process across an `exec` — the one thing in this crate that cannot be
//! spelled safely, since only the caller can know that an integer names a
//! descriptor it owns. It stats the descriptor before claiming it, so the
//! claim is checked rather than asserted. That is why the crate says `deny`
//! rather than `forbid`, and nothing else in it is permitted the same
//! latitude.

#![deny(unsafe_code)]

// Only `reject_dupes` needs it, and that exists only when some transport
// can actually be mounted.
#[cfg(any(feature = "ws", feature = "uds", feature = "iroh"))]
use std::collections::HashSet;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use peerline::runtime::Peer;
// Only the coordinator client builds these, and it is uds-only.
#[cfg(feature = "uds")]
use peerline_contract::coord::ServiceEntry;
use peerline_contract::manifest::{MANIFEST_OUT_ENV, Manifest, ServiceManifest};
// `Endpoints` and `HostId` are in scope through the re-export below.
use peerline_contract::{Consumed, Produced};
use tracing::{info, warn};

mod bindings;
#[cfg(feature = "uds")]
mod coordinator;
mod error;
mod hostid;
#[cfg(unix)]
mod lifeline;
mod serving;

pub use bindings::{Bindings, Connection};
pub use error::{BindError, DialError, HostError, PlanError};
pub use hostid::host_id;
// The two types `Connection::dial` cannot be called without. Re-exported
// because that entry point exists precisely for a caller who has *not* taken
// part in contract resolution — and telling such a caller to add a second
// crate to its manifest to name the arguments of the one function it wants
// would put the tax back that the function was added to remove.
pub use peerline_contract::{Endpoints, HostId};
// The two variables a supervisor injects into every child it spawns (§10.4).
// Re-exported here because this is the crate a service already depends on:
// `Host::coordinator_from_env` reads the first, and a service picking its
// mount address is expected to honour the second.
pub use peerline_contract::env::{COORD_SOCK_ENV, SERVICE_UDS_ENV};

// The accept loops and the rule for combining them. A host keeps serving as
// long as anything is listening, which is a policy rather than a detail of
// whichever `futures` combinator the serving stage happened to reach for —
// see [`serving`].
use serving::{TransportServe, serve_all};

// One mount entry per transport. Only defined when that transport is
// compiled in: `Mount` has no setter for a disabled one, so nothing can be
// mounted on it and its whole assembly path is gone.
#[cfg(feature = "ws")]
type WsMount = (String, peerline_transport_ws::Acceptor);
#[cfg(feature = "uds")]
type UdsMount = (PathBuf, peerline_transport_uds::Acceptor);
#[cfg(feature = "iroh")]
type IrohMount = (Vec<u8>, peerline_transport_iroh::Acceptor);

// Re-exported so a consumer configures mounts without taking a direct
// dependency on the transport crates. Each stays in its own crate because
// what it screens on is specific to that transport (see [`Remote`]).
#[cfg(feature = "iroh")]
pub use peerline_transport_iroh::{EndpointId, IrohAccept, IrohPolicy, Reachability};
#[cfg(feature = "uds")]
pub use peerline_transport_uds::{UdsAccept, UdsPolicy};
#[cfg(feature = "ws")]
pub use peerline_transport_ws::{WsAccept, WsPolicy};

/// How long a host waits for its required contracts before giving up.
///
/// There is deliberately no "wait forever": a cluster's characteristic failure
/// is silence, and a deadline is what converts silence into a diagnosis naming
/// the contracts that did not resolve.
pub const DEFAULT_AWAIT_READY: Duration = Duration::from_secs(30);

/// How long a host keeps re-resolving a dependency whose connection dropped
/// before it exits.
///
/// Exiting on the first drop would turn a routine restart of a popular producer
/// into a cluster-wide outage: its consumers exit, their consumers exit, and so
/// on down the closure. A budget keeps the crash-only property — the exit path
/// is real and regularly taken — while making an ordinary restart survivable.
pub const DEFAULT_DEPENDENCY_BUDGET: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Acceptors — where a transport's authenticated facts become a `Remote`
//
// Each is the same two-step: apply the mount's policy, and on admission
// hand back an initializer that has captured who connected. That capture is
// the whole mechanism — nothing threads identity through `peerline` itself.
// ---------------------------------------------------------------------------

#[cfg(feature = "ws")]
fn ws_acceptor(
    svc: Arc<dyn Service>,
    policy: peerline_transport_ws::WsPolicy,
) -> peerline_transport_ws::Acceptor {
    use peerline_transport_ws::{WsAccept, WsAdmit};
    Arc::new(move |accept: &WsAccept| {
        policy.check(accept)?;
        let remote = Remote::Ws(accept.clone());
        let svc = svc.clone();
        Ok(WsAdmit::new(move |peer: &Peer| {
            svc.register_remote(peer, &remote);
        }))
    })
}

#[cfg(feature = "uds")]
fn uds_acceptor(
    svc: Arc<dyn Service>,
    policy: peerline_transport_uds::UdsPolicy,
) -> peerline_transport_uds::Acceptor {
    use peerline_transport_uds::UdsAccept;
    Arc::new(move |accept: &UdsAccept| {
        policy.check(accept)?;
        let remote = Remote::Uds(*accept);
        let svc = svc.clone();
        Ok(Arc::new(move |peer: &Peer| {
            svc.register_remote(peer, &remote);
        }) as peerline_transport_uds::PeerHandler)
    })
}

#[cfg(feature = "iroh")]
fn iroh_acceptor(
    svc: Arc<dyn Service>,
    policy: peerline_transport_iroh::IrohPolicy,
) -> peerline_transport_iroh::Acceptor {
    use peerline_transport_iroh::IrohAccept;
    Arc::new(move |accept: &IrohAccept| {
        policy.check(accept)?;
        let remote = Remote::Iroh(accept.clone());
        let svc = svc.clone();
        Ok(Arc::new(move |peer: &Peer| {
            svc.register_remote(peer, &remote);
        }) as peerline_transport_iroh::PeerHandler)
    })
}

/// A composable peerline service: a name (for logging), what it produces and
/// consumes, and the handler registrations that run on each freshly-connected
/// peer.
///
/// Migrating an existing service is mechanical: its old
/// `register_handlers(peer, state)` body becomes [`Service::register`], and
/// its state lives in the implementing struct. Every method but
/// [`name`](Self::name) and [`register`](Self::register) has a default, so a
/// service that takes no part in a cluster is unchanged.
pub trait Service: Send + Sync + 'static {
    /// Stable name, used in logs. The wire address comes from [`Mount`],
    /// not from here.
    fn name(&self) -> &'static str;

    /// The contracts this service implements.
    ///
    /// A contract is a *capability*, not an instance: several processes may
    /// produce the same one, and a coordinator may hand any ready producer to
    /// any consumer. Producers of one contract at one version must therefore be
    /// interchangeable — see [`peerline_contract`].
    fn produces(&self) -> Vec<Produced> {
        Vec::new()
    }

    /// The contracts this service depends on.
    ///
    /// Declaring one makes a coordinator mandatory: the host waits for every
    /// required contract to resolve before it serves anything, and a host with
    /// no [`Host::coordinator`] fails at [`Host::run`] rather than hanging.
    fn consumes(&self) -> Vec<Consumed> {
        Vec::new()
    }

    /// Take delivery of the resolved dependencies.
    ///
    /// Called **once**, after every required contract resolved and before any
    /// transport accepts, so a handler can never observe a dependency that is
    /// not there yet. A reconnect to the coordinator deliberately does not call
    /// it again — a second `OnceLock::set` would be a silent no-op.
    ///
    /// ```ignore
    /// struct FsService { root: PathBuf, storage: OnceLock<Bindings> }
    ///
    /// impl Service for FsService {
    ///     fn name(&self) -> &'static str { "fs" }
    ///     fn produces(&self) -> Vec<Produced> { vec![Produced::new("fs", 1)] }
    ///     fn consumes(&self) -> Vec<Consumed> { vec![Consumed::new("storage", 1)] }
    ///     fn bind(&self, b: &Bindings) { let _ = self.storage.set(b.clone()); }
    ///     fn register(&self, peer: &Peer) { /* handlers dial through self.storage */ }
    /// }
    /// ```
    ///
    /// Storing the whole [`Bindings`] rather than one [`Binding`] is the
    /// better default: it is what re-dials after a producer restart pick up the
    /// *re-resolved* coordinates.
    ///
    /// A service holding a long-lived connection spawns that from here (this
    /// method is synchronous), and the loop has three steps rather than two:
    /// dial, wait for [`Connection::closed`], then wait for
    /// [`Bindings::refreshed`] before dialling again. The third is what stops
    /// it racing the host's re-resolve and re-dialling the coordinates that
    /// just died. If the dependency never comes back, the host exits when its
    /// budget is spent and the task dies with the process.
    ///
    /// [`Binding`]: peerline_contract::Binding
    fn bind(&self, bindings: &Bindings) {
        let _ = bindings;
    }

    /// Register this service's request / notification / stream handlers on
    /// a freshly-connected peer. Called once per connection.
    fn register(&self, peer: &Peer);

    /// Register handlers knowing who connected. Defaults to
    /// [`register`](Self::register), so existing services need no change.
    ///
    /// Override it to vary *what exists* per caller rather than checking
    /// permissions inside each handler:
    ///
    /// ```ignore
    /// fn register_remote(&self, peer: &Peer, remote: &Remote) {
    ///     self.register(peer);                       // read-only ops, everyone
    ///     if self.trusted(remote) {
    ///         peer.on_request("admin.wipe", ...);    // only for some
    ///     }
    /// }
    /// ```
    ///
    /// An op that was never registered answers `MethodNotFound`, which
    /// does not even confirm it exists — and there is no check to forget.
    fn register_remote(&self, peer: &Peer, remote: &Remote) {
        let _ = remote;
        self.register(peer);
    }
}

/// Who is on the other end, as the transport that accepted them
/// authenticated it.
///
/// Deliberately not unified into one `Identity`: the three carry
/// different *kinds* of claim with different weight. An `Origin` is
/// asserted by a browser about a page and is worth nothing against a
/// native client; a uid is kernel-attested; an endpoint id is proved by a
/// handshake. Flattening them would invite policy that treats a hint like
/// a proof.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Remote {
    /// A WebSocket handshake. See [`WsAccept::origin`] for what its
    /// `Origin` does and does not establish.
    ///
    /// [`WsAccept::origin`]: peerline_transport_ws::WsAccept::origin
    #[cfg(feature = "ws")]
    Ws(peerline_transport_ws::WsAccept),
    /// A Unix socket peer, with kernel-attested credentials.
    #[cfg(feature = "uds")]
    Uds(peerline_transport_uds::UdsAccept),
    /// An iroh peer, with a handshake-proved endpoint id.
    #[cfg(feature = "iroh")]
    Iroh(peerline_transport_iroh::IrohAccept),
}

/// Where one service is mounted, per transport. A `None` field means the
/// service is not exposed on that transport. Build with the fluent
/// setters; every address is explicit.
#[derive(Default, Clone)]
pub struct Mount {
    /// WebSocket path on the shared port, with the policy screening it.
    #[cfg(feature = "ws")]
    ws: Option<(String, peerline_transport_ws::WsPolicy)>,
    /// Unix domain socket file. Its parent directory is created if missing.
    #[cfg(feature = "uds")]
    uds: Option<(PathBuf, peerline_transport_uds::UdsPolicy)>,
    /// iroh ALPN on the shared endpoint, with the policy screening it.
    #[cfg(feature = "iroh")]
    iroh: Option<(Vec<u8>, peerline_transport_iroh::IrohPolicy)>,
}

impl Mount {
    /// An empty mount — not on any transport until a setter is called.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve this service at `path` on the shared WebSocket port, admitting
    /// the connections `policy` allows. Only active if the host also has a
    /// [`Host::ws_bind`] address.
    ///
    /// The policy is a required argument rather than an option with a
    /// default: WebSocket is the transport a browser can reach, and there
    /// is no default that is right for both a public app and a localhost
    /// daemon. `WsPolicy::allow_any()` is available and says so out loud.
    #[cfg(feature = "ws")]
    #[must_use]
    pub fn ws(mut self, path: impl Into<String>, policy: peerline_transport_ws::WsPolicy) -> Self {
        self.ws = Some((path.into(), policy));
        self
    }

    /// Serve this service on the Unix socket file `path`, admitting the
    /// same user (and root) — which the socket's `0600` permissions already
    /// enforce at the kernel, so this is defence in depth rather than a
    /// choice. Use [`uds_policy`](Self::uds_policy) for anything else.
    #[cfg(feature = "uds")]
    #[must_use]
    pub fn uds(mut self, path: impl Into<PathBuf>) -> Self {
        self.uds = Some((path.into(), peerline_transport_uds::UdsPolicy::same_user()));
        self
    }

    /// Serve this service on `path`, screened by `policy`. Replaces any
    /// socket set by a previous [`uds`](Self::uds) or `uds_policy` on this
    /// mount — the two are alternatives, not a setter and a modifier.
    #[cfg(feature = "uds")]
    #[must_use]
    pub fn uds_policy(
        mut self,
        path: impl Into<PathBuf>,
        policy: peerline_transport_uds::UdsPolicy,
    ) -> Self {
        self.uds = Some((path.into(), policy));
        self
    }

    /// Serve this service under `alpn` on the shared iroh endpoint,
    /// admitting the endpoints `policy` allows. Only active if the host
    /// also has a [`Host::iroh_endpoint`].
    ///
    /// Required, like the WebSocket policy, and for a sharper reason: an
    /// iroh ticket is an *address*, not a credential — it travels through
    /// logs, pastes and QR codes — so "reached us" cannot be the default
    /// authorization. `IrohPolicy::any_ticket_holder()` is available when
    /// that really is the intent.
    #[cfg(feature = "iroh")]
    #[must_use]
    pub fn iroh(
        mut self,
        alpn: impl Into<Vec<u8>>,
        policy: peerline_transport_iroh::IrohPolicy,
    ) -> Self {
        self.iroh = Some((alpn.into(), policy));
        self
    }
}

/// One mounted service, resolved out of the builder for the run.
struct Prepared {
    name: &'static str,
    /// Read once per transport; with every transport compiled out there is
    /// nothing left to read it.
    #[cfg_attr(
        not(any(feature = "ws", feature = "uds", feature = "iroh")),
        allow(dead_code)
    )]
    mount: Mount,
    svc: Arc<dyn Service>,
}

/// Builder for a unified multi-service peerline host.
#[derive(Default)]
pub struct Host {
    services: Vec<(Arc<dyn Service>, Mount)>,
    ws_bind: Option<SocketAddr>,
    iroh_key: Option<Option<PathBuf>>,
    iroh_relays: Vec<String>,
    iroh_port: Option<u16>,
    #[cfg(feature = "iroh")]
    iroh_reachability: peerline_transport_iroh::Reachability,
    coordinator: Option<PathBuf>,
    instance: Option<String>,
    await_timeout: Option<Duration>,
    dependency_budget: Option<Duration>,
}

impl Host {
    /// Start an empty host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount a service at the given address(es).
    #[must_use]
    pub fn mount(mut self, svc: impl Service, mount: Mount) -> Self {
        self.services.push((Arc::new(svc), mount));
        self
    }

    /// Bind the shared WebSocket port. Services with a [`Mount::ws`] path
    /// are served here; without this, WS mounts are inactive.
    #[must_use]
    pub fn ws_bind(mut self, addr: SocketAddr) -> Self {
        self.ws_bind = Some(addr);
        self
    }

    /// Enable the shared iroh endpoint, persisting its identity at
    /// `key_path` (`None` ⇒ ephemeral). Services with a [`Mount::iroh`]
    /// ALPN are served here.
    #[must_use]
    pub fn iroh_endpoint(mut self, key_path: Option<PathBuf>) -> Self {
        self.iroh_key = Some(key_path);
        self
    }

    /// Add a custom iroh relay (repeatable). Both ends of a link must share
    /// the same relay; a self-hosted relay is what makes the iroh transport
    /// reachable where n0's public relays are distant or unreliable and direct
    /// connectivity is impossible. No relays ⇒ n0's defaults. Only takes effect
    /// with an [`Host::iroh_endpoint`]; the URL is validated at [`Host::run`].
    #[must_use]
    pub fn iroh_relay(mut self, url: impl Into<String>) -> Self {
        self.iroh_relays.push(url.into());
        self
    }

    /// Bind the shared iroh endpoint on a fixed UDP port (all interfaces)
    /// instead of an ephemeral one. A fixed port keeps a ticket's direct
    /// addresses valid across restarts (an ephemeral port is re-picked each
    /// boot, so old tickets' direct addresses go stale — the relay and
    /// identity stay valid regardless). `0` means ephemeral. Only takes effect
    /// with an [`Host::iroh_endpoint`].
    #[must_use]
    pub fn iroh_port(mut self, port: u16) -> Self {
        self.iroh_port = Some(port);
        self
    }

    /// How far the shared iroh endpoint's ticket has to reach.
    ///
    /// Defaults to [`Reachability::Global`]: the bind waits for a relay and
    /// **fails** without one, because a ticket is an address handed to
    /// someone and one that silently works only on-LAN fails confusingly at
    /// the far end instead of loudly here.
    ///
    /// [`Reachability::LocalOnly`] is the opt-out for a deployment that is
    /// one machine, or one LAN, with no route out: the endpoint publishes a
    /// direct-address ticket immediately and never waits on a relay. The
    /// coordinates it registers are then dialable from this machine or LAN
    /// and nowhere else — see [`Reachability`] for exactly what such a
    /// ticket does and does not promise. Only takes effect with an
    /// [`Host::iroh_endpoint`].
    #[cfg(feature = "iroh")]
    #[must_use]
    pub fn iroh_reachability(mut self, reachability: Reachability) -> Self {
        self.iroh_reachability = reachability;
        self
    }

    /// Join the cluster coordinated at `socket`: register this host's
    /// coordinates, heartbeat, wait for its consumed contracts to resolve, and
    /// announce readiness once it is serving.
    ///
    /// **Opt-in, deliberately.** Without this call a host makes no outbound
    /// connection at all and never blocks on anything, which is what lets the
    /// coordinator's own host — and a supervisor's control service — bind and
    /// serve without registering with themselves. A host whose services declare
    /// [`Service::consumes`] but which never calls this fails at
    /// [`Host::run`], because the alternative is a service that hangs forever
    /// waiting for a resolution that cannot arrive.
    ///
    /// `peerline_contract::coord::DEFAULT_SOCKET` is the conventional path.
    /// Requires the `uds` feature: a coordinator is uds-only.
    #[must_use]
    pub fn coordinator(mut self, socket: impl Into<PathBuf>) -> Self {
        self.coordinator = Some(socket.into());
        self
    }

    /// Join the coordinator named by [`COORD_SOCK_ENV`], if it is set.
    ///
    /// **This is the injection contract, stated (§10.4).** Nothing in a
    /// cluster config reaches a child unless the supervisor puts it there: the
    /// file declares the coordinator's socket and each service's `uds`, and a
    /// child process cannot see either. The supervisor therefore exports
    /// `PEERLINE_COORD_SOCK` (and [`SERVICE_UDS_ENV`]) into every child it
    /// spawns, and this is the half a service links against — without it, a
    /// service keeps reading a coordinator address out of its own config and
    /// the cluster file's addresses are decorative.
    ///
    /// Unset leaves the host exactly as it was, which is the point: the same
    /// binary run by hand, with no supervisor and no variable, makes no
    /// outbound connection and does not wait for anybody (§10.7). A service
    /// that consumes contracts and is started without the variable still fails
    /// loudly at [`Host::run`] rather than hanging.
    ///
    /// An empty value is treated as unset — an exported-but-blank variable is
    /// a supervisor or shell artefact, never a request to dial `""`.
    #[must_use]
    pub fn coordinator_from_env(self) -> Self {
        match peerline_contract::env::var(COORD_SOCK_ENV) {
            Some(v) => self.coordinator(PathBuf::from(v)),
            None => self,
        }
    }

    /// Override the instance id this host registers under.
    ///
    /// The default is derived from the mounted service names and their
    /// configured addresses, which is stable across restarts of either side —
    /// so one service keeps one identity in logs, in a dashboard and in a CLI.
    /// Set it explicitly when two hosts would otherwise derive the same id, or
    /// when an operator wants to choose the name.
    #[must_use]
    pub fn instance(mut self, id: impl Into<String>) -> Self {
        self.instance = Some(id.into());
        self
    }

    /// How long to wait for required contracts before failing the run.
    /// Defaults to [`DEFAULT_AWAIT_READY`].
    #[must_use]
    pub fn await_ready_timeout(mut self, timeout: Duration) -> Self {
        self.await_timeout = Some(timeout);
        self
    }

    /// How long to keep re-resolving a dependency whose connection dropped
    /// before exiting. Defaults to [`DEFAULT_DEPENDENCY_BUDGET`].
    #[must_use]
    pub fn dependency_budget(mut self, budget: Duration) -> Self {
        self.dependency_budget = Some(budget);
        self
    }

    /// What this host would report if probed: every mounted service and the
    /// contracts it declares. See [`peerline_contract::manifest`].
    pub fn manifest(&self) -> Manifest {
        Manifest {
            services: self
                .services
                .iter()
                .map(|(svc, _)| ServiceManifest {
                    service: svc.name().to_string(),
                    produces: svc.produces(),
                    consumes: svc.consumes(),
                })
                .collect(),
        }
    }

    /// Bind every configured transport and serve until a **termination
    /// signal** — `SIGINT` (Ctrl-C) or, on unix, `SIGTERM`. See
    /// [`Host::run_until`] to supply a shutdown future of your own.
    ///
    /// `SIGTERM` is not a nicety. A supervisor stops its children with it
    /// (§9, §10.5), and its default disposition kills the process outright —
    /// so a host that waited only on `ctrl_c()` never reached its own shutdown
    /// path under a supervisor, and the graceful path was exercised solely by
    /// a human pressing Ctrl-C. Both signals resolve the same future and
    /// [`Host::run`] returns `Ok(())`: a clean stop, distinct from the
    /// non-zero "restart me" of an error.
    ///
    /// A **lifeline**, if this process was given one, is a third way to reach
    /// that same path — and unlike the signals it does not depend on anybody
    /// remembering to send anything. See [`Host::run_until`].
    pub async fn run(self) -> Result<(), HostError> {
        self.run_until(terminate()).await
    }

    /// Bind every configured transport and serve until `shutdown` resolves
    /// or the run fails.
    ///
    /// Errors if no service is registered, no mount is active on any
    /// configured transport, two services collide on one address, a service
    /// consumes a contract with no [`Host::coordinator`] configured, the
    /// required contracts never resolve, or a dependency stays gone past the
    /// [`dependency budget`](Host::dependency_budget). An error is the "restart
    /// me" signal: map it to a non-zero exit, which is what distinguishes it
    /// from the clean `Ok(())` of a shutdown signal.
    ///
    /// One thing that is deliberately **not** in that list: a transport whose
    /// accept loop dies while serving. A host keeps serving as long as
    /// anything is listening, and only the last live transport's failure ends
    /// the run — with the coordinates already published for the dead transport
    /// left stale. [`BoundHost::serve_until`] documents the rule and what it
    /// costs; this method is that method with the binding in front of it.
    ///
    /// With `PEERLINE_MANIFEST_OUT` set it binds nothing, writes the manifest
    /// to that path and returns `Ok(())`.
    ///
    /// # The lifeline is not one of the caller's choices
    ///
    /// `shutdown` replaces the *signals* [`Host::run`] would have waited on,
    /// because which signal means "stop" is the caller's business. The
    /// lifeline is watched here regardless, because it is not a policy: it is
    /// the answer to "is the process that started me still alive", and a host
    /// that supplied its own shutdown future has not thereby volunteered to
    /// outlive its supervisor. A caller that wants a drain on the way out gets
    /// it either way — the lifeline resolves this method exactly as its own
    /// future would, with the same `Ok(())`.
    ///
    /// With no `PEERLINE_LIFELINE_FD` in the environment there is no lifeline
    /// and this is `shutdown` alone, which is how every host not started by a
    /// supervisor runs.
    pub async fn run_until(self, shutdown: impl Future<Output = ()>) -> Result<(), HostError> {
        // The configuration checks and the manifest probe run *before* the
        // shutdown race, exactly as they always have. They are synchronous and
        // they touch nothing, so there is nothing for a shutdown to save by
        // pre-empting them — and a host that is misconfigured has to say so
        // even when it is asked to stop in the same breath, or a caller that
        // passes an already-resolved shutdown (which every test of these
        // errors does) gets a clean `Ok` out of a broken configuration.
        let Some((prepared, plan)) = self.prepare()? else {
            return Ok(());
        };

        // From here the shutdown wins over everything, including a bind that
        // has not finished: an iroh endpoint takes seconds to bind and may
        // fail, and a host asked to stop must not first wait that out.
        tokio::pin!(shutdown);
        let bound = tokio::select! {
            biased;
            () = &mut shutdown => {
                info!("peerline-host: shutdown signal received, stopping");
                return Ok(());
            }
            r = self.bind_planned(prepared, plan) => r?,
        };

        match bound {
            // Neither claimed anything and neither will serve, and both are a
            // clean exit — a probe did its job, and an orphan has nobody left
            // to serve. (`Probed` cannot arrive here: `prepare` answered it
            // above. Spelling it out beats a wildcard that would silently
            // absorb a future variant.)
            Bound::Probed | Bound::Orphaned => Ok(()),
            Bound::Listening(host) => host.serve_until(shutdown).await,
        }
    }

    /// Bind every configured transport and stop there, so the caller can read
    /// the addresses that were **actually** bound before anything is served.
    ///
    /// # Why this is a separate step
    ///
    /// `bind(2)` and `accept(2)` are separate syscalls, and the window between
    /// them is where a host learns what it is. A host that asked for
    /// `127.0.0.1:0` does not know its port until the kernel has picked one;
    /// an iroh endpoint has no ticket until it is bound and has found a relay.
    /// [`run`](Self::run) resolves both and then hands them only to the
    /// coordinator — so a caller that needs its own address for anything else
    /// (reporting it in a handshake, writing it to a file, passing it to a
    /// child) had no way to get it, and the only way out was to stop using
    /// `Host` and re-implement the acceptor, the policy wiring and the mount
    /// table against the transport crate directly.
    ///
    /// ```ignore
    /// let Bound::Listening(bound) = host.bind().await? else { return Ok(()) };
    /// if let Some(addr) = bound.ws_addr() {
    ///     service.publish(format!("ws://{addr}/"));   // the real port
    /// }
    /// bound.serve_until(terminate()).await
    /// ```
    ///
    /// # Where the split sits, and why exactly there
    ///
    /// `Bind -> Register -> AwaitReady -> Service::bind(deps) -> Serve -> Ready`
    ///
    /// The cut is immediately after `Bind` and the endpoint report built from
    /// it, and before **everything** to its right. That position is forced
    /// from both sides. It cannot be earlier: the endpoints are computed from
    /// what bound, which is the whole reason `Bind` comes first, and there is
    /// nothing to report before that. It cannot be later: the very next step
    /// registers those coordinates with the coordinator, and a caller that
    /// wanted to *add* to them — a ws URL of its own, a port it passes to a
    /// child — has to do so before they are published, or it publishes one
    /// thing and tells the cluster another.
    ///
    /// So everything that can fail without touching the network still happens
    /// here (the manifest probe, the missing-coordinator check, the plan), and
    /// everything that involves another process still happens in
    /// [`BoundHost::serve_until`]. The listeners are open across the gap, which
    /// is the same property the coordinator handshake already relied on: a
    /// client that dials while the caller is still deciding queues in the
    /// kernel backlog rather than being refused.
    ///
    /// # The three outcomes
    ///
    /// Two of them are not a host to serve, and they are variants rather than
    /// a silent `Ok` because a caller holding a [`BoundHost`] is about to do
    /// something with an address, and "there is no address" must not be
    /// spelled the same way as "here it is". See [`Bound`].
    pub async fn bind(self) -> Result<Bound, HostError> {
        let Some((prepared, plan)) = self.prepare()? else {
            return Ok(Bound::Probed);
        };
        self.bind_planned(prepared, plan).await
    }

    /// Everything that can be decided without touching the network: the
    /// configuration errors, and the manifest probe.
    ///
    /// `Ok(None)` is the probe — it has already written its manifest, and
    /// there is nothing left to bind. Otherwise the mount tables, ready to
    /// bind.
    ///
    /// Separate from [`bind_planned`](Self::bind_planned) because these checks
    /// must not be skippable. [`run_until`](Self::run_until) races the bind
    /// against the caller's shutdown, and a host handed a shutdown that has
    /// already resolved would otherwise never discover that it was
    /// misconfigured — it would answer `Ok(())` to a duplicate mount address
    /// or an unreadable iroh key. Nothing here blocks, so there is nothing for
    /// a shutdown to gain by pre-empting it.
    fn prepare(&self) -> Result<Option<(Vec<Prepared>, Plan)>, HostError> {
        if self.services.is_empty() {
            return Err(HostError::NoServices);
        }

        // The probe (§10.3): report and exit, before anything is bound and
        // before any coordinate is claimed.
        // Through the shared helper, so that an exported-but-blank
        // `PEERLINE_MANIFEST_OUT` is *unset* — the rule `peerline-contract`
        // states for every one of these variables. `var_os` alone made a blank
        // value a request to write the manifest to `""`, which fails, and
        // fails before anything binds: one stray `export` and every host in
        // the cluster refuses to start.
        if let Some(out) = peerline_contract::env::var(MANIFEST_OUT_ENV) {
            write_manifest(&self.manifest(), Path::new(&out))?;
            return Ok(None);
        }

        // The service itself, not a pre-boxed initializer: each transport
        // builds its own acceptor, because the initializer now closes over
        // the identity that transport authenticated.
        let prepared: Vec<Prepared> = self
            .services
            .iter()
            .map(|(svc, mount)| Prepared {
                name: svc.name(),
                mount: mount.clone(),
                svc: svc.clone(),
            })
            .collect();

        // A dependency with nowhere to resolve it is a configuration error, and
        // it must be *loud*: the alternative is a service that binds, never
        // becomes ready, and never says why.
        if self.coordinator.is_none()
            && let Some(p) = prepared.iter().find(|p| !p.svc.consumes().is_empty())
        {
            return Err(HostError::NoCoordinator { service: p.name });
        }

        // Everything that can be decided without touching the network is
        // decided here, so a bad key, a bad relay URL or two services sharing
        // an address fails the run rather than racing the shutdown signal.
        let plan = self.plan(&prepared)?;
        if plan.is_empty() {
            return Err(HostError::NoMounts);
        }

        info!(services = self.services.len(), "peerline-host starting");
        Ok(Some((prepared, plan)))
    }

    /// The half of [`bind`](Self::bind) that touches the network: take the
    /// lifeline, claim the addresses, and build the endpoint report from what
    /// actually bound.
    async fn bind_planned(self, prepared: Vec<Prepared>, plan: Plan) -> Result<Bound, HostError> {
        // Taken *after* the probe and the plan, so that a manifest probe and a
        // misconfiguration both still behave exactly as they did — and before
        // anything binds, so a supervisor that is already gone is discovered
        // before this process claims an address.
        //
        // Boxed and carried into the `BoundHost` rather than taken again
        // later, because `lifeline::take` yields the descriptor **once per
        // process**: a second call answers `Absent`, so a lifeline that was
        // taken here and dropped would leave the served host watching nothing.
        // That is the one piece of state the split cannot simply re-derive.
        let mut lifeline: BoxFuture<'static, ()> = Box::pin(lifeline_closed());

        let host_id = hostid::host_id().clone();
        let listening = tokio::select! {
            biased;
            () = &mut lifeline => {
                info!("peerline-host: the supervisor is gone before this host bound, \
                       stopping");
                return Ok(Bound::Orphaned);
            }
            r = bind_all(plan) => r?,
        };
        let endpoints = self.endpoints(&prepared, &listening, &host_id);

        Ok(Bound::Listening(Box::new(BoundHost {
            host: self,
            prepared,
            listening,
            endpoints,
            host_id,
            lifeline,
        })))
    }

    #[cfg(feature = "uds")]
    async fn with_coordinator(
        &self,
        prepared: &[Prepared],
        endpoints: Vec<Endpoints>,
        serves: Vec<TransportServe>,
        host: HostId,
        socket: PathBuf,
    ) -> Result<(), HostError> {
        use coordinator::CoordCtx;

        let entries: Vec<ServiceEntry> = prepared
            .iter()
            .zip(endpoints)
            .map(|(p, endpoints)| ServiceEntry {
                service: p.name.to_string(),
                endpoints,
                produces: p.svc.produces(),
                consumes: p.svc.consumes(),
            })
            .collect();
        let awaits = entries.iter().any(|e| !e.consumes.is_empty());
        let ctx = Arc::new(CoordCtx {
            socket: socket.clone(),
            instance: self.instance_id(prepared),
            pid: std::process::id(),
            entries,
            awaits,
            await_timeout: self.await_timeout.unwrap_or(DEFAULT_AWAIT_READY),
        });

        let (bindings_tx, bindings_rx) = tokio::sync::oneshot::channel();
        let (serving_tx, serving_rx) = tokio::sync::watch::channel(false);
        // The client is a future rather than a task so that dropping this host
        // — a shutdown signal — drops the coordinator connection and stops the
        // heartbeat with it. The coordinator has no deregistration op: an
        // instance leaves the registry by falling silent, and its TTL is what
        // notices.
        let client = coordinator::client(ctx.clone(), bindings_tx, serving_rx);
        tokio::pin!(client);

        // Register + AwaitReady, with the heartbeat already running beside them
        // inside the client.
        let resolved = tokio::select! {
            e = &mut client => return Err(e),
            b = bindings_rx => b.map_err(|_| {
                HostError::Coordinator(
                    "the coordinator client stopped before resolving".into(),
                )
            })?,
        };

        // The declarations as well as the bindings: a contract's optionality
        // is a property of what was *asked for*, and the watchdog below needs
        // it to know whether a spent budget is an exit or a degradation.
        let consumed: Vec<Consumed> = prepared.iter().flat_map(|p| p.svc.consumes()).collect();
        let (bindings, lost) = Bindings::new(host, &consumed, resolved);
        for p in prepared {
            p.svc.bind(&bindings);
        }

        let watchdog = coordinator::watch_dependencies(
            lost,
            bindings.clone(),
            socket,
            ctx.instance.clone(),
            self.dependency_budget.unwrap_or(DEFAULT_DEPENDENCY_BUDGET),
        );

        // Serving starts now; readiness is announced by the client as soon as
        // it sees this. The listeners have been open since `Bind`, so anything
        // that dialled during the resolving window is already queued.
        let _ = serving_tx.send(true);

        tokio::select! {
            r = serve_all(serves) => r,
            e = &mut client => Err(e),
            e = watchdog => Err(e),
        }
    }

    #[cfg(not(feature = "uds"))]
    async fn with_coordinator(
        &self,
        _prepared: &[Prepared],
        _endpoints: Vec<Endpoints>,
        _serves: Vec<TransportServe>,
        _host: HostId,
        _socket: PathBuf,
    ) -> Result<(), HostError> {
        Err(HostError::Coordinator(
            "Host::coordinator needs the `uds` feature (a coordinator is uds-only)".into(),
        ))
    }

    /// Assemble every transport's mount table. Sync: nothing here touches the
    /// network, and everything here is a configuration error if it fails.
    #[cfg_attr(
        not(any(feature = "ws", feature = "uds", feature = "iroh")),
        allow(unused_variables, unused_mut)
    )]
    fn plan(&self, prepared: &[Prepared]) -> Result<Plan, PlanError> {
        let mut plan = Plan::default();

        #[cfg(feature = "ws")]
        if let Some(addr) = self.ws_bind {
            let mut mounts: Vec<WsMount> = Vec::new();
            for p in prepared {
                if let Some((path, policy)) = &p.mount.ws {
                    mounts.push((path.clone(), ws_acceptor(p.svc.clone(), policy.clone())));
                }
            }
            reject_dupes("ws path", mounts.iter().map(|(p, _)| p.clone()))?;
            if !mounts.is_empty() {
                plan.ws = Some((addr, mounts));
            }
        }

        #[cfg(feature = "uds")]
        {
            let mut mounts: Vec<UdsMount> = Vec::new();
            for p in prepared {
                if let Some((path, policy)) = &p.mount.uds {
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| PlanError::UdsDirectory {
                            path: dir.to_path_buf(),
                            cause: e.to_string(),
                        })?;
                    }
                    mounts.push((path.clone(), uds_acceptor(p.svc.clone(), policy.clone())));
                }
            }
            reject_dupes(
                "uds socket",
                mounts.iter().map(|(p, _)| p.display().to_string()),
            )?;
            plan.uds = mounts;
        }

        #[cfg(feature = "iroh")]
        if let Some(key_path) = &self.iroh_key {
            let mut mounts: Vec<IrohMount> = Vec::new();
            for p in prepared {
                if let Some((alpn, policy)) = &p.mount.iroh {
                    mounts.push((alpn.clone(), iroh_acceptor(p.svc.clone(), policy.clone())));
                }
            }
            reject_dupes("iroh alpn", mounts.iter().map(|(a, _)| a.clone()))?;
            if !mounts.is_empty() {
                // The identity is resolved here, not at bind: serving under a
                // silently different key would invalidate every ticket ever
                // handed out, so a broken key file must fail the run.
                let key = peerline_transport_iroh::load_or_create_secret_key(key_path.as_deref())
                    .map_err(|e| PlanError::IrohKey(e.to_string()))?;
                let mut config =
                    peerline_transport_iroh::IrohConfig::from_relay_urls(self.iroh_relays.clone())
                        .map_err(|e| PlanError::IrohRelay(e.to_string()))?;
                config.bind_port = self.iroh_port;
                config.reachability = self.iroh_reachability;
                plan.iroh = Some(IrohPlan {
                    key,
                    config,
                    mounts,
                });
            }
        }

        Ok(plan)
    }

    /// This host's dial coordinates, per mounted service — built from what
    /// actually bound, which is the whole reason `Bind` comes first.
    #[cfg_attr(
        not(any(feature = "ws", feature = "uds", feature = "iroh")),
        allow(unused_variables, unused_mut)
    )]
    fn endpoints(
        &self,
        prepared: &[Prepared],
        listening: &Listening,
        host: &HostId,
    ) -> Vec<Endpoints> {
        let mut out: Vec<Endpoints> = prepared
            .iter()
            .map(|_| Endpoints::new(host.clone()))
            .collect();

        #[cfg(feature = "ws")]
        if let Some(bound) = &listening.ws {
            let addr = bound.addr();
            for (i, p) in prepared.iter().enumerate() {
                if let Some((path, _)) = &p.mount.ws {
                    let url = format!("ws://{addr}{path}");
                    info!(service = p.name, url = %url, "peerline-host: reachable (ws)");
                    out[i].ws.push(url);
                }
            }
        }

        #[cfg(feature = "uds")]
        if listening.uds.is_some() {
            for (i, p) in prepared.iter().enumerate() {
                if let Some((path, _)) = &p.mount.uds {
                    info!(service = p.name, socket = %path.display(),
                          "peerline-host: reachable (uds)");
                    out[i].uds.push(path.display().to_string());
                }
            }
        }

        #[cfg(feature = "iroh")]
        if let Some(bound) = &listening.iroh {
            let ticket = bound.ticket().to_string();
            for (i, p) in prepared.iter().enumerate() {
                if let Some((alpn, _)) = &p.mount.iroh {
                    let alpn = String::from_utf8_lossy(alpn).into_owned();
                    info!(service = p.name, ticket, alpn = %alpn,
                          "peerline-host: reachable (iroh — dial ticket with this alpn)");
                    out[i].iroh.push(peerline_contract::IrohDial {
                        ticket: ticket.clone(),
                        alpn,
                    });
                }
            }
        }

        // A service whose every transport failed to bind is still registered —
        // its `consumes` is part of what gates this host's readiness, and
        // dropping the entry would silently drop that — but it is advertised
        // with no coordinates, so nothing can bind to it by mistake. Say so:
        // this is a service that is running and unreachable.
        for (p, e) in prepared.iter().zip(&out) {
            if e.ws.is_empty() && e.uds.is_empty() && e.iroh.is_empty() {
                warn!(
                    service = p.name,
                    "peerline-host: no transport bound for this service; it is \
                     registered but not reachable"
                );
            }
        }

        out
    }

    /// A stable identity for this host: the services it mounts and the
    /// addresses they were *configured* with.
    ///
    /// Configured rather than resolved, deliberately — an ephemeral ws port
    /// changes every boot, and an id that changed with it would show one
    /// service as a new instance after every restart, which is exactly what a
    /// stable id exists to prevent.
    #[cfg(feature = "uds")]
    fn instance_id(&self, prepared: &[Prepared]) -> String {
        if let Some(id) = &self.instance {
            return id.clone();
        }
        let mut names: Vec<&str> = Vec::new();
        let mut coords: Vec<String> = Vec::new();
        for p in prepared {
            names.push(p.name);
            #[cfg(feature = "ws")]
            if let Some((path, _)) = &p.mount.ws {
                coords.push(format!("ws:{:?}{path}", self.ws_bind));
            }
            #[cfg(feature = "uds")]
            if let Some((path, _)) = &p.mount.uds {
                coords.push(format!("uds:{}", path.display()));
            }
            #[cfg(feature = "iroh")]
            if let Some((alpn, _)) = &p.mount.iroh {
                coords.push(format!("iroh:{}", String::from_utf8_lossy(alpn)));
            }
        }
        coords.sort();
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        names.hash(&mut hasher);
        coords.hash(&mut hasher);
        format!("{}-{:08x}", names.join("+"), hasher.finish() as u32)
    }
}

// ---------------------------------------------------------------------------
// The bind / serve split
// ---------------------------------------------------------------------------

/// What [`Host::bind`] found when it stopped between `bind` and `accept`.
///
/// Three outcomes, because a host has three honest answers to "are you
/// listening": yes, and here is where; no, because I was only being asked what
/// I am; and no, because the process that started me is already gone. The two
/// `no`s are **not** errors — each is a clean exit, and a caller that treats
/// them as failures would turn a routine probe into a restart loop.
///
/// A caller that only wants the listening case gets all three right in one
/// line, which is why they are one enum rather than an `Option` and a flag:
///
/// ```ignore
/// let Bound::Listening(bound) = host.bind().await? else { return Ok(()) };
/// ```
#[non_exhaustive]
pub enum Bound {
    /// Bound, published, and not yet accepting. The addresses are readable and
    /// [`BoundHost::serve_until`] takes it from here.
    ///
    /// Boxed because this variant is two orders of magnitude larger than the
    /// other two, and a `Result<Bound, _>` is returned by value.
    Listening(Box<BoundHost>),
    /// `PEERLINE_MANIFEST_OUT` was set, so this run was a **probe**: the
    /// manifest has been written to that path and nothing was bound. Exit 0.
    ///
    /// This is how a supervisor learns the contract graph from the binary
    /// itself — see [`peerline_contract::manifest`].
    Probed,
    /// The **lifeline** closed before the bind finished: the supervisor that
    /// spawned this process is gone. Nothing was claimed, and serving would
    /// mean outliving it. Exit 0 — this is a stop that was asked for, not a
    /// failure. See [`peerline_contract::lifeline`].
    Orphaned,
}

/// A host whose transports are bound and published, but not yet accepting.
///
/// The addresses here are the ones the kernel actually gave out, which is the
/// whole point: a `ws_bind` of `127.0.0.1:0` has a real port by now, and an
/// iroh endpoint has a real ticket. Read them, do whatever has to happen
/// before anybody connects, then call [`serve`](Self::serve) or
/// [`serve_until`](Self::serve_until).
///
/// Nothing is being accepted while this is held, but the listeners are open:
/// a client that dials meanwhile queues in the kernel backlog instead of being
/// refused. That is the same property the coordinator handshake has always
/// depended on, now available to a caller as well.
pub struct BoundHost {
    /// The builder, kept whole: everything to the right of `Bind` — the
    /// coordinator socket, the instance id, the two timeouts — is still read
    /// off it, and copying those fields out would be the same struct twice.
    host: Host,
    prepared: Vec<Prepared>,
    listening: Listening,
    endpoints: Vec<Endpoints>,
    host_id: HostId,
    /// Taken during [`Host::bind`] and carried, not re-taken: the descriptor
    /// is claimed once per process.
    lifeline: BoxFuture<'static, ()>,
}

impl BoundHost {
    /// Every mounted service's dial coordinates, in the order the services
    /// were [`mounted`](Host::mount).
    ///
    /// This is exactly what would be registered with a coordinator, built from
    /// what bound rather than from what was configured — a transport that
    /// failed to bind contributes nothing here, so nothing in it is a
    /// coordinate that does not work.
    pub fn endpoints(&self) -> &[Endpoints] {
        &self.endpoints
    }

    /// One service's coordinates by name, for a caller that would otherwise
    /// have to count mounts to find them.
    ///
    /// `None` if no service of that name is mounted. Two services cannot share
    /// a name usefully — the name is what a log line and a manifest identify
    /// them by — so the first match is the answer.
    pub fn endpoints_for(&self, service: &str) -> Option<&Endpoints> {
        let i = self.prepared.iter().position(|p| p.name == service)?;
        self.endpoints.get(i)
    }

    /// The address the shared WebSocket port is really bound to.
    ///
    /// `None` when no ws port was configured, or when it failed to bind and
    /// the host is serving on its other transports. This is the one thing
    /// [`endpoints`](Self::endpoints) cannot give back as a `SocketAddr`: a
    /// coordinate is a URL, and a caller that wants the port wants the number.
    #[cfg(feature = "ws")]
    pub fn ws_addr(&self) -> Option<SocketAddr> {
        self.listening.ws.as_ref().map(|b| b.addr())
    }

    /// The shared iroh endpoint's ticket, or `None` if iroh was not configured
    /// or did not bind.
    ///
    /// One ticket for the whole host; which service a dialer reaches is picked
    /// by the ALPN, which is in each service's [`endpoints`](Self::endpoints).
    #[cfg(feature = "iroh")]
    pub fn iroh_ticket(&self) -> Option<&str> {
        self.listening.iroh.as_ref().map(|b| b.ticket())
    }

    /// Every unix socket path this host is listening on, across all services.
    ///
    /// The paths were configured rather than discovered, so this says nothing
    /// new about *where* — what it says is that they bound, which the caller
    /// could not otherwise know.
    #[cfg(feature = "uds")]
    pub fn uds_paths(&self) -> Vec<&str> {
        self.endpoints
            .iter()
            .flat_map(|e| e.uds.iter().map(String::as_str))
            .collect()
    }

    /// Accept connections until a **termination signal** — `SIGINT` or, on
    /// unix, `SIGTERM` — exactly as [`Host::run`] would have.
    pub async fn serve(self) -> Result<(), HostError> {
        self.serve_until(terminate()).await
    }

    /// Accept connections until `shutdown` resolves, the lifeline closes, or
    /// the run fails.
    ///
    /// This is everything to the right of `Bind`: register with a coordinator
    /// if one was configured, wait for required contracts,
    /// [`Service::bind`] the resolved dependencies in, start accepting, and
    /// announce readiness. The lifeline taken during [`Host::bind`] is watched
    /// here for the same reason [`Host::run_until`] watches it — it is not one
    /// of the caller's choices.
    ///
    /// # A host serves as long as anything is listening
    ///
    /// An accept loop that dies mid-serve does not end this future. The
    /// transport is dropped with a `warn!` naming it and the error, and the
    /// rest go on answering; only when the **last** live transport's loop ends
    /// does this resolve, with that loop's own result. The rule is the same
    /// for ws, uds and iroh — a ws port lost to whatever took it does not cost
    /// a caller the unix socket that every local process was reaching it on,
    /// which is otherwise a whole session lost to a door closing.
    ///
    /// This is about the accept loop, not the bind. A transport that never
    /// bound is already gone by here: [`Host::bind`] drops it, and it
    /// contributes no coordinate to [`endpoints`](Self::endpoints).
    ///
    /// # What that costs: the endpoints for a dead transport go stale
    ///
    /// A host registers its coordinates with the coordinator once, at `Ready`,
    /// and a transport dying afterwards does **not** withdraw them. A consumer
    /// resolving this service can be handed a ws URL or an iroh ticket that no
    /// longer answers, and finds out by failing to connect. Withdrawing them
    /// would mean re-registering a shrunken set mid-serve, which is a question
    /// about what a re-registration means for consumers already bound to the
    /// old set — a coordinator-side protocol question, not something this
    /// method can decide on its own. The staleness is the accepted cost: it
    /// fails one dial, where ending the host failed every transport at once,
    /// including the ones that were still working.
    pub async fn serve_until(self, shutdown: impl Future<Output = ()>) -> Result<(), HostError> {
        let Self {
            host,
            prepared,
            listening,
            endpoints,
            host_id,
            mut lifeline,
        } = self;
        let serves = listening.into_serves();

        let work = async move {
            match &host.coordinator {
                None => {
                    // No coordinator: `Bind -> Serve`, and nothing else. `bind`
                    // is still called, with an empty set, so a service can rely
                    // on it having run exactly once before any handler does.
                    //
                    // No declarations either, and that is exact rather than
                    // lazy: `bind` has already refused any service that
                    // consumes anything when there is no coordinator to resolve
                    // it with.
                    let (bindings, _lost) = Bindings::new(host_id, &[], Vec::new());
                    for p in &prepared {
                        p.svc.bind(&bindings);
                    }
                    serve_all(serves).await
                }
                Some(socket) => {
                    host.with_coordinator(&prepared, endpoints, serves, host_id, socket.clone())
                        .await
                }
            }
        };
        tokio::pin!(shutdown);
        tokio::pin!(work);

        tokio::select! {
            biased;
            () = &mut shutdown => {
                info!("peerline-host: shutdown signal received, stopping");
                Ok(())
            }
            () = &mut lifeline => {
                info!("peerline-host: the supervisor is gone, stopping");
                Ok(())
            }
            r = &mut work => r,
        }
    }
}

// ---------------------------------------------------------------------------
// Bind
// ---------------------------------------------------------------------------

/// Every transport's mount table, assembled and ready to bind.
#[derive(Default)]
struct Plan {
    #[cfg(feature = "ws")]
    ws: Option<(SocketAddr, Vec<WsMount>)>,
    #[cfg(feature = "uds")]
    uds: Vec<UdsMount>,
    #[cfg(feature = "iroh")]
    iroh: Option<IrohPlan>,
}

#[cfg(feature = "iroh")]
struct IrohPlan {
    key: peerline_transport_iroh::SecretKey,
    config: peerline_transport_iroh::IrohConfig,
    mounts: Vec<IrohMount>,
}

impl Plan {
    #[cfg_attr(
        not(any(feature = "ws", feature = "uds", feature = "iroh")),
        allow(unused_mut)
    )]
    fn is_empty(&self) -> bool {
        let mut empty = true;
        #[cfg(feature = "ws")]
        {
            empty &= self.ws.is_none();
        }
        #[cfg(feature = "uds")]
        {
            empty &= self.uds.is_empty();
        }
        #[cfg(feature = "iroh")]
        {
            empty &= self.iroh.is_none();
        }
        empty
    }
}

/// The listeners, bound and published but not yet accepting.
#[derive(Default)]
struct Listening {
    #[cfg(feature = "ws")]
    ws: Option<peerline_transport_ws::Bound>,
    #[cfg(feature = "uds")]
    uds: Option<peerline_transport_uds::Bound>,
    #[cfg(feature = "iroh")]
    iroh: Option<peerline_transport_iroh::Bound>,
}

impl Listening {
    #[cfg_attr(
        not(any(feature = "ws", feature = "uds", feature = "iroh")),
        allow(unused_mut)
    )]
    fn is_empty(&self) -> bool {
        let mut empty = true;
        #[cfg(feature = "ws")]
        {
            empty &= self.ws.is_none();
        }
        #[cfg(feature = "uds")]
        {
            empty &= self.uds.is_none();
        }
        #[cfg(feature = "iroh")]
        {
            empty &= self.iroh.is_none();
        }
        empty
    }

    #[cfg_attr(
        not(any(feature = "ws", feature = "uds", feature = "iroh")),
        allow(unused_mut)
    )]
    /// Every bound transport's accept loop, each labelled with the transport
    /// it belongs to.
    ///
    /// Nothing here decides what a failure means — [`serve_all`] does, for all
    /// of them at once. That used to be the other way round: iroh's arm caught
    /// its own error and parked forever while ws's and uds's propagated, so
    /// the host's failure policy was three different policies depending on
    /// which future you read. The label is all that is left of the difference,
    /// and it exists only so the log line can name the transport.
    fn into_serves(self) -> Vec<TransportServe> {
        let mut serves: Vec<TransportServe> = Vec::new();
        #[cfg(feature = "ws")]
        if let Some(bound) = self.ws {
            serves.push(TransportServe::new(
                "ws",
                Box::pin(async move { bound.serve().await.map_err(HostError::Serve) }),
            ));
        }
        #[cfg(feature = "uds")]
        if let Some(bound) = self.uds {
            serves.push(TransportServe::new(
                "uds",
                Box::pin(async move { bound.serve().await.map_err(HostError::Serve) }),
            ));
        }
        #[cfg(feature = "iroh")]
        if let Some(bound) = self.iroh {
            serves.push(TransportServe::new(
                "iroh",
                Box::pin(async move { bound.serve().await.map_err(HostError::Serve) }),
            ));
        }
        serves
    }
}

/// Bind every planned transport.
///
/// **A transport that fails to bind is dropped**, and the host goes on with
/// what did bind — so a ws+uds+iroh host on an offline machine still serves ws
/// and uds. What it must never do is advertise a coordinate that does not work,
/// which is why the endpoint report is built from this and not from the plan.
/// If *nothing* bound there is nothing to serve, and that is the same failure
/// as having configured no mounts at all.
#[cfg_attr(
    not(any(feature = "ws", feature = "uds", feature = "iroh")),
    allow(unused_variables, unused_mut)
)]
async fn bind_all(plan: Plan) -> Result<Listening, BindError> {
    let mut listening = Listening::default();
    let mut failures: Vec<String> = Vec::new();

    #[cfg(feature = "ws")]
    if let Some((addr, mounts)) = plan.ws {
        match peerline_transport_ws::bind_mounted(addr, mounts).await {
            Ok(bound) => listening.ws = Some(bound),
            Err(e) => failures.push(format!("ws {addr}: {e}")),
        }
    }

    #[cfg(feature = "uds")]
    if !plan.uds.is_empty() {
        match peerline_transport_uds::bind_mounted(plan.uds).await {
            Ok(bound) => listening.uds = Some(bound),
            Err(e) => failures.push(format!("uds: {e}")),
        }
    }

    #[cfg(feature = "iroh")]
    if let Some(iroh) = plan.iroh {
        // Under the default [`Reachability::Global`] this is seconds-scale
        // and network-dependent: it waits for the endpoint to come online
        // and then for a relay, and fails without one, because a ticket that
        // cannot be dialed off-LAN is worse than an error. Under
        // [`Host::iroh_reachability`]`(LocalOnly)` it is milliseconds and
        // needs no network — the ticket is this machine's direct addresses.
        match peerline_transport_iroh::bind_mounted(iroh.key, iroh.config, iroh.mounts).await {
            Ok(bound) => listening.iroh = Some(bound),
            Err(e) => failures.push(format!("iroh: {e}")),
        }
    }

    if listening.is_empty() {
        return Err(BindError { failures });
    }
    for failure in failures {
        warn!(
            transport = %failure,
            "peerline-host: transport failed to bind; serving without it and not \
             advertising it"
        );
    }
    Ok(listening)
}

/// Write the probe's answer and exit — no binds, no coordinator, no
/// coordinates. A file rather than stdout, which the service's own logging is
/// already using by the time it reaches here.
fn write_manifest(manifest: &Manifest, path: &Path) -> Result<(), HostError> {
    let json = manifest.to_json().map_err(|e| HostError::Manifest {
        path: path.to_path_buf(),
        cause: format!("cannot serialise the manifest: {e}"),
    })?;
    std::fs::write(path, json).map_err(|e| HostError::Manifest {
        path: path.to_path_buf(),
        cause: e.to_string(),
    })?;
    info!(path = %path.display(), "peerline-host: wrote manifest and exiting");
    Ok(())
}

/// Resolve when the supervisor that spawned this process goes away — or
/// never, if nothing is supervising it.
///
/// A property of the *process*, not of any one [`Host`]: the descriptor is
/// taken once, and a second host in the same process sees the same "absent"
/// it would see on a machine with no supervisor.
///
/// The `unix` gate is the one the module carries. A platform without file
/// descriptors has no lifeline, which is the supported absent case and not a
/// degradation.
async fn lifeline_closed() {
    #[cfg(unix)]
    match lifeline::take() {
        lifeline::Lifeline::Held(fd) => return lifeline::closed(fd).await,
        lifeline::Lifeline::Absent => {}
        lifeline::Lifeline::Unusable(why) => warn!(
            reason = %why,
            variable = peerline_contract::lifeline::LIFELINE_FD_ENV,
            "peerline-host: the lifeline variable is set but unusable; running without one"
        ),
    }
    // Absent, unusable, or not unix. Waiting on nothing is the whole of
    // "behaves exactly as it always has": a future that resolved instead would
    // stop the host the moment it started.
    std::future::pending::<()>().await;
}

/// Resolve on `SIGINT` or, on unix, `SIGTERM` — whichever arrives first.
///
/// A supervisor sends `SIGTERM`; a terminal sends `SIGINT`. Both mean "stop",
/// and the host's shutdown path is the same for either. Installing the
/// `SIGTERM` handler is also what *disarms* its default disposition: without
/// it the kernel terminates the process where it stands and none of the code
/// below the signal ever runs.
///
/// A handler that cannot be installed is logged and dropped rather than
/// failing the run: losing one of two signals is a degraded stop, and refusing
/// to serve at all over it would be worse.
async fn terminate() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(e) => {
                warn!(error = %e, "peerline-host: cannot listen for SIGTERM; SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Reject two services sharing one address on a transport — that would let
/// one silently shadow the other (and axum panics on a duplicate path).
#[cfg(any(feature = "ws", feature = "uds", feature = "iroh"))]
fn reject_dupes<T: std::hash::Hash + Eq + std::fmt::Debug>(
    kind: &'static str,
    keys: impl Iterator<Item = T>,
) -> Result<(), PlanError> {
    let mut seen = HashSet::new();
    for key in keys {
        if !seen.insert(format!("{key:?}")) {
            return Err(PlanError::AddressCollision {
                transport: kind,
                address: format!("{key:?}"),
            });
        }
    }
    Ok(())
}
