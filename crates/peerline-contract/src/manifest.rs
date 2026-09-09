//! The manifest envelope: what a probed binary reports about itself.
//!
//! Contracts are declared in a service's `Service` impl, so the only authority
//! on what a binary produces and consumes is the binary. A manifest file
//! shipped beside it would be a second copy of the same facts and would drift
//! the first time a dependency changed. Instead the supervisor asks:
//!
//! ```text
//! PEERLINE_MANIFEST_OUT=/tmp/probe.json  fs-service
//! ```
//!
//! `Host::run()` sees the variable, writes this JSON to that **path**, and
//! exits 0 without binding anything.
//!
//! # Why an environment variable, and why a file
//!
//! A CLI flag cannot work: `Host` never sees `argv`. Real services parse
//! arguments in the first statement of `main` and exit non-zero on anything
//! unrecognised, so `fs-service --peerline-manifest` would exit before a `Host`
//! existed. An environment variable passes through any argv parser untouched.
//!
//! The output goes to a file rather than stdout because by the time `Host::run`
//! is reached the service has already initialised logging, and a manifest on
//! stdout would have to compete with it.
//!
//! # Why it lives here
//!
//! Both halves of the probe are outside this crate and neither may depend on
//! the other: `peerline-host` writes the file and `peerline-cluster` reads it,
//! and the dependency arrow between those two points from the cluster layer to
//! the host, never back. Spelling the envelope in either one would force the
//! other to re-spell it — the same reason [`crate::coord`] holds the `coord.*`
//! wire rather than the coordinator.
//!
//! # Why it is a list
//!
//! One process may mount several services, each with its own coordinates, and
//! `coord.register` already carries a `Vec`. An envelope with room for one name
//! would contradict that, so the file is a [`Manifest`] — `{ "services": [ … ]
//! }` — even when, as today, most binaries mount exactly one thing.
//!
//! A supervisor keys everything on the **config** name rather than on any name
//! inside the manifest: the config is what it can act on, and one binary may be
//! run twice under two entries.

use serde::{Deserialize, Serialize};

use crate::{Consumed, Produced};

/// The environment variable that asks a peerline binary to report its manifest
/// instead of serving: its value is the **path to write to**.
pub const MANIFEST_OUT_ENV: &str = "PEERLINE_MANIFEST_OUT";

/// Everything one binary declares about itself: one entry per mounted service.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The services this process mounts, in mount order.
    #[serde(default)]
    pub services: Vec<ServiceManifest>,
}

impl Manifest {
    /// An empty manifest — a binary that mounts nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// The common case: a process mounting exactly one service.
    pub fn single(service: ServiceManifest) -> Self {
        Self {
            services: vec![service],
        }
    }

    /// Add one mounted service.
    pub fn with(mut self, service: ServiceManifest) -> Self {
        self.services.push(service);
        self
    }

    /// Serialise to the exact bytes a probed binary is expected to write.
    ///
    /// Provided so that the writing side and the reading side cannot disagree
    /// about the encoding, and so tests and fixtures can produce a real one.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parse a manifest written by a probed binary.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Every contract any of the mounted services produces, in order.
    pub fn produces(&self) -> impl Iterator<Item = &Produced> {
        self.services.iter().flat_map(|s| s.produces.iter())
    }

    /// Every contract any of the mounted services consumes, in order.
    pub fn consumes(&self) -> impl Iterator<Item = &Consumed> {
        self.services.iter().flat_map(|s| s.consumes.iter())
    }
}

/// What one mounted service declares about itself.
///
/// Deliberately *not* the same shape as a coordinator registration: there are
/// no endpoints here. A probe runs a process that never binds anything, so it
/// has no coordinates to report — addresses come from the config, contracts
/// come from the code, and this type is the code half.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceManifest {
    /// The name the binary knows this service by — its mount name.
    ///
    /// Informational. A supervisor keys everything on the config's `name`
    /// instead, because one binary may legitimately be run twice under two
    /// different entries. A mismatch between the two is worth showing in a
    /// diagnostic and is never an error.
    pub service: String,
    /// The contracts this service implements.
    #[serde(default)]
    pub produces: Vec<Produced>,
    /// The contracts it depends on. `optional: true` entries do not gate
    /// readiness and do not gate validation.
    #[serde(default)]
    pub consumes: Vec<Consumed>,
}

impl ServiceManifest {
    /// A manifest for a service that declares nothing yet. Useful in tests, and
    /// the honest answer for a binary that has not been ported to declare
    /// contracts: it produces nothing, so nothing can depend on it, and it
    /// consumes nothing, so it constrains no ordering.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            produces: Vec::new(),
            consumes: Vec::new(),
        }
    }

    /// Add a produced contract.
    pub fn producing(mut self, produced: Produced) -> Self {
        self.produces.push(produced);
        self
    }

    /// Add a consumed contract.
    pub fn consuming(mut self, consumed: Consumed) -> Self {
        self.consumes.push(consumed);
        self
    }

    /// Serialise one service's declaration. The file a probe reads is a
    /// [`Manifest`]; this is here for tests and for building one up.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parse one service's declaration.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}
