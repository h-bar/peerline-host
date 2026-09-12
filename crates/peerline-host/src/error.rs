//! What can go wrong, as types a caller can match on.
//!
//! Every surface in this crate used to return `Result<_, String>`. A string is
//! the right thing to *show* an operator and the wrong thing to hand a
//! program: a supervisor deciding whether to restart a child, a service
//! deciding whether re-dialling is worth trying, a test asserting that the
//! right thing failed — all of them were reduced to `err.contains("...")`,
//! which couples them to wording that was never a promise.
//!
//! # The taxonomy, and why it is four types rather than one
//!
//! The split follows the questions, not the call graph. A caller holding one
//! of these is asking exactly one thing, and the four things are different:
//!
//! - [`PlanError`] — *this host is configured wrong.* Decided before anything
//!   is bound and before any coordinate is claimed, so it is reproducible: the
//!   same binary with the same configuration fails the same way every time.
//!   Never worth a retry; always worth an operator.
//! - [`BindError`] — *this host cannot claim its addresses.* The port is busy,
//!   the socket directory is gone, the machine is offline. Environmental, and
//!   the one class where retrying later is the obvious response.
//! - [`DialError`] — *I cannot reach that producer.* Asked by a service about
//!   one dependency, not by a supervisor about this process, which is why it
//!   is not a `HostError` variant. It carries
//!   [`ConnectError`](peerline::runtime::ConnectError) where the cause was a
//!   refusal, because a refusal is the one dial failure that will not fix
//!   itself.
//! - [`HostError`] — *the run ended.* The union, plus the failures that belong
//!   to the run as a whole (the coordinator refused us, a required dependency
//!   never came back, an accept loop died).
//!
//! # `Other` is deliberate, and deliberately not load-bearing
//!
//! Each enum ends in an `Other(String)` for the long tail — a transport's own
//! error prose, an io failure with nothing to decide about it. The rule is
//! that no *decision* may rest on `Other`: if a caller needs to tell two
//! `Other`s apart, that is the signal to promote one to a variant. It exists
//! so that adding a failure mode is not a breaking change, not so that the
//! taxonomy can stop growing.
//!
//! # `From<...> for String`, on every one of them
//!
//! This crate's error type was `String` for its whole life, and downstream
//! code is full of `?` into functions that return `Result<_, String>`. Each
//! type therefore converts into one, so those sites keep compiling and the
//! change is additive. What that conversion cannot reach is a site that
//! *names* `String` — a `JoinHandle<Result<(), String>>`, a function whose
//! declared return type is `Result<_, String>` with no `?` on the path — and
//! those are one-line annotation changes rather than rewrites.
//!
//! # Why these are `#[non_exhaustive]` when generated contract types are not
//!
//! `peerline-contract`'s generated enums are closed on purpose: they are wire
//! shapes, serde rejects a value outside the compiled set at the decode
//! boundary, and openness would only buy a wildcard arm that hides a manifest
//! change. None of that applies here. These are not decoded from anything,
//! there is no boundary that rejects an unknown variant, and a new failure
//! mode is a normal thing for a host to grow. The reasoning differs because
//! the situation does, not because the rule was forgotten.

use std::path::PathBuf;

use peerline::runtime::ConnectError;
use peerline_contract::ContractId;

// ---------------------------------------------------------------------------
// Plan — decided before anything is bound
// ---------------------------------------------------------------------------

/// A configuration error, found before any address was claimed.
///
/// Everything here is a property of how this host was *built* — the mounts it
/// was given, the key file it was pointed at, the relay URLs it was handed —
/// so it fails identically on every run until somebody changes something.
/// That is the whole reason the plan is assembled before the first bind: a
/// misconfiguration that surfaced halfway through binding would leave some
/// addresses claimed and some not.
#[non_exhaustive]
#[derive(Debug)]
pub enum PlanError {
    /// Two services were mounted at one address on one transport, which would
    /// let one silently shadow the other.
    AddressCollision {
        /// Which transport's namespace collided — `"ws path"`, `"uds socket"`
        /// or `"iroh alpn"`.
        transport: &'static str,
        /// The address both services asked for.
        address: String,
    },
    /// A unix socket's parent directory could not be created.
    UdsDirectory {
        /// The directory that could not be made.
        path: PathBuf,
        /// What the filesystem said.
        cause: String,
    },
    /// The iroh secret key could not be loaded or created.
    ///
    /// Fatal rather than recoverable: serving under a silently different key
    /// invalidates every ticket this host ever handed out.
    IrohKey(String),
    /// An iroh relay URL was not a URL.
    IrohRelay(String),
    /// The long tail. See the module docs — nothing may branch on this.
    Other(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AddressCollision { transport, address } => {
                write!(f, "two services share {transport} {address}")
            }
            Self::UdsDirectory { path, cause } => {
                write!(f, "uds dir {}: {cause}", path.display())
            }
            Self::IrohKey(e) => write!(f, "iroh key: {e}"),
            Self::IrohRelay(e) => write!(f, "iroh relay: {e}"),
            Self::Other(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for PlanError {}

// ---------------------------------------------------------------------------
// Bind — claiming the addresses
// ---------------------------------------------------------------------------

/// Every configured transport failed to bind, so there is nothing to serve.
///
/// A transport that fails to bind while another succeeds is **not** this: the
/// host goes on with what bound, warns, and simply does not advertise the one
/// that did not. This is the case where nothing at all is left.
#[non_exhaustive]
#[derive(Debug)]
pub struct BindError {
    /// One sentence per transport that failed, in the order they were tried.
    pub failures: Vec<String>,
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no active mounts — every configured transport failed to bind ({})",
            self.failures.join("; ")
        )
    }
}

impl std::error::Error for BindError {}

// ---------------------------------------------------------------------------
// Dial — reaching one producer
// ---------------------------------------------------------------------------

/// Why a producer could not be reached.
///
/// Asked by a service about one dependency. The distinction that earns this a
/// type of its own is [`Refused`](Self::Refused) versus
/// [`Unreachable`](Self::Unreachable): a refusal is the producer's admission
/// policy saying no, which is deterministic and will say no again, while
/// everything else is a coordinate that is wrong or a producer that is down —
/// exactly the case the host's re-resolve exists for. A caller that retries
/// both treats a permissions bug as a network blip.
#[non_exhaustive]
#[derive(Debug)]
pub enum DialError {
    /// This contract has no binding: it was never declared in any service's
    /// `consumes`, or it is optional and has not resolved yet.
    ///
    /// Not a dependency that broke — there is nothing for the host to
    /// re-resolve — which is why reaching this reports no loss.
    NotBound {
        /// The contract that has no binding.
        contract: ContractId,
    },
    /// The producer's admission policy rejected this dialer.
    ///
    /// Deterministic: the same dial will be refused again. Something has to
    /// change at the producer (its `WsPolicy`, `UdsPolicy` or `IrohPolicy`) or
    /// at this dialer (its origin, its uid, its endpoint id).
    Refused {
        /// Which coordinate refused us, for the log line.
        endpoint: String,
        /// The transport's own account of the refusal.
        cause: ConnectError,
    },
    /// Every coordinate was tried and none answered.
    Unreachable {
        /// One sentence per attempt, in preference order (uds, ws, iroh).
        attempts: Vec<String>,
    },
    /// The producer reported no coordinates this binary can dial — it
    /// advertised nothing, or advertised only transports this build left out.
    NoCoordinates {
        /// What it did advertise, as a sentence.
        advertised: String,
    },
    /// The long tail. See the module docs — nothing may branch on this.
    Other(String),
}

impl DialError {
    /// Whether trying again, unchanged, could ever work.
    ///
    /// `false` for a refusal and for a contract that was never declared:
    /// both need a change somewhere before the same call can succeed. `true`
    /// for everything else, which is the case the host's re-resolve and a
    /// service's re-dial loop are built around.
    pub fn is_transient(&self) -> bool {
        !matches!(self, Self::Refused { .. } | Self::NotBound { .. })
    }
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotBound { contract } => write!(f, "no binding for `{contract}`"),
            Self::Refused { endpoint, cause } => {
                write!(f, "{endpoint} refused this dialer: {cause}")
            }
            Self::Unreachable { attempts } => f.write_str(&attempts.join("; ")),
            Self::NoCoordinates { advertised } => f.write_str(advertised),
            Self::Other(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for DialError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused { cause, .. } => Some(cause),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Host — the run as a whole
// ---------------------------------------------------------------------------

/// Why a host did not start, or did not stay up.
///
/// An error from [`Host::run`](crate::Host::run) is the **"restart me"**
/// signal: map it to a non-zero exit, which is what distinguishes it from the
/// clean `Ok(())` of a shutdown signal or a lifeline EOF. Which of these is
/// worth restarting *for* is the caller's call, and having the variants is
/// what makes that call possible — a [`Plan`](Self::Plan) will fail the same
/// way forever, a [`Bind`](Self::Bind) may not.
#[non_exhaustive]
#[derive(Debug)]
pub enum HostError {
    /// No service was mounted, so there is nothing to serve.
    NoServices,
    /// A service declares `consumes` but no coordinator was configured, so
    /// nothing can ever resolve it.
    ///
    /// Loud and immediate by design: the alternative is a host that binds,
    /// never becomes ready, and never says why.
    NoCoordinator {
        /// The first service found declaring a contract it consumes.
        service: &'static str,
    },
    /// This run was a manifest probe (`PEERLINE_MANIFEST_OUT`) and the
    /// manifest could not be written.
    Manifest {
        /// Where it was to be written.
        path: PathBuf,
        /// What stopped it.
        cause: String,
    },
    /// Misconfiguration, found before anything was bound.
    Plan(PlanError),
    /// Nothing is mounted on any transport that was configured — no
    /// `ws_bind`, no `iroh_endpoint`, no `Mount` with an address.
    NoMounts,
    /// Every configured transport failed to bind.
    Bind(BindError),
    /// The coordinator refused this host, or stopped in a way a reconnect
    /// cannot fix: a duplicate instance id, a dependency that never resolved,
    /// a coordinator that never answered `coord.await_ready`.
    ///
    /// A coordinator that merely went away is **not** here — that is
    /// reconnected with backoff and never reaches a caller.
    Coordinator(String),
    /// A required contract stayed unresolvable for the whole
    /// [`dependency budget`](crate::Host::dependency_budget).
    ///
    /// Only ever a **required** one: losing an optional contract degrades this
    /// host and never ends it.
    DependencyLost {
        /// The contract that did not come back.
        contract: ContractId,
        /// The last thing that went wrong while re-resolving it.
        reason: String,
    },
    /// A transport's accept loop ended in failure while the host was serving.
    Serve(String),
    /// The long tail. See the module docs — nothing may branch on this.
    Other(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Every message carries the crate prefix, because these are read in a
        // log beside a dozen other crates' lines and the wording is otherwise
        // unattributable.
        f.write_str("peerline-host: ")?;
        match self {
            Self::NoServices => f.write_str("no services registered"),
            Self::NoCoordinator { service } => write!(
                f,
                "`{service}` consumes contracts but no coordinator is configured — \
                 call Host::coordinator(socket)"
            ),
            Self::Manifest { path, cause } => {
                write!(f, "cannot write {}: {cause}", path.display())
            }
            Self::Plan(e) => write!(f, "{e}"),
            Self::NoMounts => f.write_str(
                "no active mounts (configure ws_bind / iroh_endpoint and give each \
                 service a Mount)",
            ),
            Self::Bind(e) => write!(f, "{e}"),
            Self::Coordinator(e) => f.write_str(e),
            Self::DependencyLost { contract, reason } => {
                write!(f, "`{contract}` {reason}; exiting")
            }
            Self::Serve(e) => f.write_str(e),
            Self::Other(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Plan(e) => Some(e),
            Self::Bind(e) => Some(e),
            _ => None,
        }
    }
}

impl From<PlanError> for HostError {
    fn from(e: PlanError) -> Self {
        Self::Plan(e)
    }
}

impl From<BindError> for HostError {
    fn from(e: BindError) -> Self {
        Self::Bind(e)
    }
}

// Keeps `?` working in every downstream that returns `Result<_, String>` —
// the error type this crate's whole public surface had before it was typed.
// See the module docs for the one shape this cannot absorb.
macro_rules! into_string {
    ($($t:ty),*) => {
        $(
            impl From<$t> for String {
                fn from(e: $t) -> Self {
                    e.to_string()
                }
            }
        )*
    };
}

into_string!(HostError, PlanError, BindError, DialError);
