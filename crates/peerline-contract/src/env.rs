//! The environment variables a supervisor injects into every child it spawns.
//!
//! **Nothing in a cluster config reaches a child unless the supervisor puts it
//! there (§10.4).** The config declares the coordinator's socket and each
//! service's `uds`, but a child process cannot see the file — it is handed an
//! `argv` its own parser owns and an environment. So the addresses travel in
//! the environment, and these are their names.
//!
//! # Why here, and not in the supervisor
//!
//! The same reason [`crate::coord`] holds the `coord.*` wire and
//! [`crate::manifest`] holds the probe envelope: both ends are outside this
//! crate and neither may depend on the other. `peerline-cluster` exports these
//! variables and `peerline-host` (and every service linking it) reads them,
//! and the dependency arrow runs from the cluster layer to the host, never
//! back. Spelled in the supervisor they would be a private detail, which is
//! exactly the state this module exists to end: while they were, real services
//! kept reading the coordinator address from their own config files and the
//! cluster file's `uds` stayed decorative, describing an address nothing
//! actually bound.
//!
//! # The contract
//!
//! - [`COORD_SOCK_ENV`] — the coordinator to register with. A host honours it
//!   by calling `Host::coordinator_from_env()`. **Unset means no coordinator**,
//!   which is a valid configuration (§10.7) and not an error.
//! - [`SERVICE_UDS_ENV`] — the Unix socket this process should mount its
//!   service on. A *convention* rather than something `Host` can enforce: the
//!   address belongs to the `Mount`, which the service builds, and a process
//!   mounting several services has to distribute one injected address itself.
//!   A service that honours it makes the config's `uds` the real address; one
//!   that ignores it silently disagrees with the file an operator is reading.
//! - [`MANIFEST_OUT_ENV`] — the probe (see [`crate::manifest`]). Named here
//!   too so that every variable a supervisor sets is in one list.
//! - [`LIFELINE_FD_ENV`] — the descriptor whose EOF means *your supervisor is
//!   gone, stop* (see [`crate::lifeline`]). **Unset means no supervisor is
//!   watching**, which is how a service run by hand or by systemd always runs,
//!   and changes nothing else about it.
//!
//! Injected values **win over an entry's own `env`**: the supervisor is
//! asserting where things actually are, and a config that also sets one of
//! these by hand is describing a cluster it is not running.
//!
//! An empty value is treated as unset everywhere. An exported-but-blank
//! variable is a shell or supervisor artefact, never a request to dial `""`.

pub use crate::lifeline::LIFELINE_FD_ENV;
pub use crate::manifest::MANIFEST_OUT_ENV;

/// The Unix socket of the coordinator a service should register with.
///
/// Read by `Host::coordinator_from_env()`; set by the supervisor from the
/// cluster config's `coordinator.uds`. Defaults, when nobody sets it, are the
/// caller's business — [`crate::coord::DEFAULT_SOCKET`] is the conventional
/// path, but a host that is given neither simply does not join a cluster.
pub const COORD_SOCK_ENV: &str = "PEERLINE_COORD_SOCK";

/// The Unix socket this process should serve its service on.
///
/// Set by the supervisor from the config entry's `uds`. A service reads it
/// when choosing its mount address, in preference to its own config, so that
/// the address in the cluster file is the address that gets bound.
pub const SERVICE_UDS_ENV: &str = "PEERLINE_SERVICE_UDS";

/// Read one of these variables, treating empty as unset.
///
/// Provided so that both ends apply the same rule rather than each writing
/// their own `var_os(..).filter(..)` and disagreeing about the blank case.
pub fn var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}
