//! Contract types for the peerline cluster layer.
//!
//! A service declares the **contracts** it produces and consumes — never the
//! instance names of its dependencies. A coordinator matches each consumed
//! contract to a live producer and hands back a [`Binding`]: which producer it
//! chose, and how to dial it. These are the shapes that travel on that control
//! path and nothing else — pure serde types, no transport, no runtime — so a
//! dashboard, a CLI or a non-Rust implementation can use them without pulling
//! in a peer. Once a consumer holds a [`Binding`] it dials the producer
//! directly and calls that producer's ops; no type here appears on the data
//! connection.
//!
//! # The interchangeability invariant
//!
//! **Producers of one contract at one version are interchangeable.** Any
//! consumer must be correct when bound to any of them. This is a precondition
//! on whoever implements a contract, not a property the coordinator can check:
//! it may hand out any ready producer, and a consumer that restarts may be
//! bound to a different one than it held before.
//!
//! The concrete consequence is a constraint on service design, not on
//! deployment: two `storage/1` producers backed by *independent* databases is a
//! contract violation, not a redundancy strategy. Producers of one contract
//! must either share backing state or be genuinely stateless. Services that are
//! not substitutable get distinct contract names.
//!
//! Nothing can *verify* that precondition, but a producer that knows it admits
//! no second instance can say so: [`Produced::cardinality`] carries
//! [`Cardinality::Single`], and a coordinator refuses the second live producer
//! at registration instead of silently ranking it second.
//!
//! # Naming
//!
//! **`Produced` / `Consumed` are participles deliberately.** peerline already
//! uses the bare nouns *producer* and *consumer* for the two ends of a stream,
//! and readiness is delivered as a stream, so both senses show up in one file.
//! The participles name *declarations*; `Producer` and `Consumer` stay reserved
//! for stream ends and are not shadowed here.
//!
//! *Produce / consume* rather than *provide / require* is also the house
//! vocabulary, and it avoids a contradiction: [`Consumed`] `{ optional: true }`
//! reads correctly where `Required { optional: true }` would not.
//!
//! # Wire format
//!
//! Every type is a plain JSON object with the field names spelled by its Rust
//! fields; the derives are the only definition. Fields with a natural empty
//! value are `#[serde(default)]`, so a producer in another language may omit
//! them. Unknown fields are ignored rather than rejected, because a contract
//! version is **additive**: see [`ContractId`].
//!
//! # The control protocol
//!
//! [`coord`] carries the `coord.*` op names and their argument / result
//! shapes: how a service registers, heartbeats, becomes ready, and waits for
//! its dependencies. It lives here rather than in the coordinator so that a
//! host can call a coordinator without depending on one, exactly as the types
//! above let a service declare a contract without depending on a registry.
//!
//! # The manifest probe
//!
//! [`manifest`] carries the envelope a binary writes when it is asked what it
//! declares, and the environment variable that asks. It is here for the same
//! reason: a host writes it, a supervisor reads it, and neither may depend on
//! the other.
//!
//! # Contract manifests
//!
//! Everything above is the *control* path: what a service declares and what a
//! coordinator hands back. A contract's own vocabulary — its ops, their
//! argument and result shapes, its error codes — is not spelled in Rust at
//! all. It is a **contract manifest**: a language-neutral JSON file, described
//! by [`spec`], from which `contract!` generates the Rust at compile time.
//! (Written as plain code rather than as a link, here and below: the macro is
//! behind the default-on `macros` feature, and a link to something a reader's
//! own feature set may have turned off is a link that does not resolve.)
//!
//! ```ignore
//! peerline_contract::contract!("codex.server/1" in "contracts");
//! ```
//!
//! A call site names the **directory** and which contract it wants out of
//! it, because the directory is the unit a contract is written in: the
//! manifests in one reference each other's shapes and share one error-code
//! space. All of them are read and checked together, so a file that does not
//! parse, an import naming a type that does not exist, or two contracts
//! picking the same error code are compile errors at every crate that
//! expands any of them.
//!
//! The generated types *are* the manifest, so they cannot drift from it; a
//! service in another language reads the same file and is checked against the
//! same thing. The reasoning that would otherwise be rustdoc lives in the
//! manifest too, because it is contract documentation rather than Rust
//! documentation, and it is emitted as doc comments on whatever is generated.
//!
//! A manifest may also be a **vocabulary**: named shapes and no ops, for
//! other manifests to import. It exists because a shape can travel in
//! several contracts, and three restatements of one shape are three things
//! that can drift with nothing saying they were ever the same. An import
//! names a contract and a version — never a Rust path, which would mean
//! nothing to an implementor who is not writing Rust — and the call site
//! says which module supplies it:
//!
//! ```ignore
//! peerline_contract::contract!(
//!     "coagent.runtime/1" in "contracts",
//!     imports { "coagent.rpc/1" => ::coagent_rpc },
//! );
//! ```
//!
//! That clause carries no correctness — whether the vocabulary exists and
//! what its shapes look like is answered by the directory — only the Rust
//! fact of which module holds its expansion.
//!
//! **Two things here are called a manifest, and they are different.**
//! [`manifest`] is what a *binary* reports when probed: which contracts each
//! of its mounted services produces and consumes. [`spec`] is what a
//! *contract* is, independent of any process that implements it.
//!
//! # The lifeline
//!
//! [`lifeline`] carries the one variable that travels the other way round the
//! same loop: a supervisor hands each child an inherited pipe and names its
//! descriptor there, and the child stops when that pipe reports EOF — which is
//! what makes it impossible for a supervised child to outlive its supervisor,
//! however the supervisor died. Here for the third time for the same reason,
//! and stated once so that the two ends cannot drift.

#![forbid(unsafe_code)]

pub mod coord;
pub mod env;
/// The dependency graph a set of contracts induces.
///
/// Here rather than in the cluster layer because two crates in two workspaces
/// derive it from two different inputs and must agree on its shape; see the
/// module's own docs. Pure `std`, no wire types, nothing serialized.
pub mod graph;
pub mod lifeline;
pub mod manifest;

/// The contract manifest format: what a contract's manifest file may say, and
/// what makes one valid.
///
/// Re-exported rather than defined here because a proc-macro crate can export
/// nothing but macros, so `contract!` cannot own the format it reads — and
/// a second copy of the format, one for the macro and one for everybody else,
/// is exactly the drift this design exists to make impossible.
pub use peerline_contract_spec as spec;

/// Expand a contract manifest into Rust: op name constants, the argument,
/// result and item types, the error codes, and the manifest's own prose as
/// doc comments.
///
/// Takes the contract wanted and the directory holding it —
/// `"codex.server/1" in "contracts"`, the path relative to the invoking
/// crate's `Cargo.toml` — and three optional clauses: `imports`, binding each
/// imported contract to the Rust module that supplies it; `typescript`,
/// naming the shapes a browser reads; and `runtime`, asking for the producer
/// trait, its registration and the client. All three are at the call site
/// rather than in the file because all three are facts about this target and
/// not about the contract. See the crate docs.
///
/// `runtime` is the one that requires something of the invoking crate: a
/// `peerline` dependency with its `runtime` feature, under that name. The
/// macro's own crate docs list every dependency name a generated expansion
/// hard-codes.
///
/// Behind the default-on `macros` feature. A crate that holds these types
/// without expanding a manifest — a dashboard, a CLI, a supervisor — turns it
/// off and stops building `syn` and `quote` for a macro it never writes.
#[cfg(feature = "macros")]
pub use peerline_contract_macros::contract;

use std::fmt;

use serde::{Deserialize, Serialize};

/// Which contract, at which version.
///
/// The registry key, the match predicate, a vertex in the cycle graph, and the
/// correlation tag on the readiness stream — always in the control path, never
/// on the data connection.
///
/// **The version is one integer major, not a semver range.** peerline's own
/// wire does exactly this (`ver: "1"`): additive within a version, a new major
/// as a new variant. One versioning discipline in the stack rather than two.
/// Range matching stays speculative until a contract actually has two live
/// majors, and the interchangeability invariant (crate docs) already forbids
/// widening a match across versions.
///
/// Splitting the name from the version is justified by error quality rather
/// than by matching. With a flat `"fs/2"` string, "no producer for `fs/2`" is
/// indistinguishable from a typo'd name; split, a coordinator can say *"`fs` is
/// produced at version 1; you consume version 2"*. Given that the failure mode
/// of a cluster is "things silently never become ready", that is worth one
/// struct.
///
/// A `ContractId` is a **promise, not a proof**: nothing verifies that a
/// producer implements what it claims. A mismatch surfaces as a
/// method-not-found at the consumer, not as a refusal at registration.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContractId {
    /// The contract name, e.g. `"storage"`. Names a *capability*, not an
    /// instance: several processes may produce the same name.
    pub name: String,
    /// The major version. Incremented only for a breaking change; additive
    /// changes keep the number.
    pub version: u32,
}

impl ContractId {
    /// A contract id from its two parts.
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        Self {
            name: name.into(),
            version,
        }
    }
}

impl fmt::Display for ContractId {
    /// Renders as `name/version` — the form used in log lines and in
    /// coordinator errors ("`fs` waited 30s for `storage/1`"). It is a display
    /// form only; the wire form is the two-field object.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.name, self.version)
    }
}

/// How many producers of one contract may be live at once.
///
/// # Why this is declared, and why here
///
/// The interchangeability invariant (crate docs) is a precondition a
/// coordinator cannot check: it has no way to know that two `storage/1`
/// producers are backed by two independent SQLite files rather than one shared
/// one. Some contracts, though, can say up front that they admit **no** second
/// producer — a gateway owning a listening port, a manager owning a process
/// table, a store owning a local database file. For those, "two producers" is
/// never redundancy and always a misconfiguration, and it is one that is
/// otherwise **silent**: a selection policy of "lowest instance id" simply
/// picks one, and the second sits there serving a different database until
/// somebody notices data going missing from one of them.
///
/// Cardinality is logically a property of the *contract*, but a contract has no
/// central definition anywhere in this system — a [`ContractId`] is just a name
/// and a major version, minted by whoever says it. So the only place the claim
/// can be made is on a [`Produced`] declaration, by a producer, which means two
/// producers of one contract can disagree about it. **A `Single` declaration
/// by either of them binds for that contract**: the strict reading wins, on the
/// same principle §6 already applies to [`Consumed::optional`], where a
/// contract consumed as required by one service and optional by another is
/// required. Refusing a second producer that should have been allowed is a
/// loud, immediate, one-line config fix; admitting one that should have been
/// refused is the silent data-loss failure the enum exists to prevent. A
/// coordinator that sees a disagreement should say so — it is a
/// misconfiguration in its own right — but it must still refuse.
///
/// A named enum rather than a `bool`, so the declaration reads as a claim
/// (`Produced::single("storage", 1)`) rather than as a knob.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    /// Any number of interchangeable producers. The default, and what every
    /// declaration that predates this field means: on the wire an absent
    /// `cardinality` reads as `Any`, which is additive within a contract
    /// version exactly as [`ContractId`] requires.
    #[default]
    Any,
    /// Exactly one producer may be registered at a time. A second live
    /// producer is refused at registration rather than ranked second.
    ///
    /// The claim is about *live* producers, not about history: once the
    /// incumbent's registration expires on its TTL, or it re-registers as
    /// itself after a restart, a producer may hold the contract again.
    Single,
}

impl Cardinality {
    /// Whether this declaration admits at most one live producer.
    pub fn is_single(self) -> bool {
        self == Self::Single
    }
}

impl fmt::Display for Cardinality {
    /// The wire spelling, for log lines and error messages.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Any => "any",
            Self::Single => "single",
        })
    }
}

/// A contract this service implements.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Produced {
    /// What is produced.
    pub contract: ContractId,
    /// The op names this producer answers for the contract.
    ///
    /// Carried but unused at first: it is the hook for checking a producer
    /// against its contract later. Reserving the field now makes that check
    /// additive rather than a wire change, which is the same reason
    /// [`Endpoints::host`] exists on day one.
    #[serde(default)]
    pub ops: Vec<String>,
    /// How many producers of this contract may be live at once. See
    /// [`Cardinality`] for why the claim lives on the producer and what
    /// happens when two producers disagree.
    ///
    /// Defaults to [`Cardinality::Any`] on the wire, so a registration written
    /// before this field existed — or by an implementation in another language
    /// that does not know about it — keeps deserialising and keeps meaning
    /// what it meant.
    #[serde(default)]
    pub cardinality: Cardinality,
}

impl Produced {
    /// Declare a produced contract with no op list, admitting any number of
    /// interchangeable producers. `ops` is advisory and unchecked, so omitting
    /// it costs nothing today.
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        Self {
            contract: ContractId::new(name, version),
            ops: Vec::new(),
            cardinality: Cardinality::Any,
        }
    }

    /// Declare a produced contract of which there may be **only one live
    /// producer** — see [`Cardinality::Single`].
    ///
    /// Spelled as its own constructor rather than as a flag on
    /// [`Self::new`] so the call site states the claim: a singleton is a fact
    /// about the contract, not a setting on this registration.
    pub fn single(name: impl Into<String>, version: u32) -> Self {
        Self::new(name, version).with_cardinality(Cardinality::Single)
    }

    /// Declare a produced contract together with the ops it answers.
    pub fn with_ops<S: Into<String>>(
        name: impl Into<String>,
        version: u32,
        ops: impl IntoIterator<Item = S>,
    ) -> Self {
        Self {
            contract: ContractId::new(name, version),
            ops: ops.into_iter().map(Into::into).collect(),
            cardinality: Cardinality::Any,
        }
    }

    /// Set the cardinality, for combining with [`Self::with_ops`].
    pub fn with_cardinality(mut self, cardinality: Cardinality) -> Self {
        self.cardinality = cardinality;
        self
    }
}

/// A contract this service depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consumed {
    /// What is consumed.
    pub contract: ContractId,
    /// Whether the service can run without it.
    ///
    /// Two moments, and the word means the same thing at both.
    ///
    /// **At startup**, a required contract gates readiness: the consumer waits,
    /// and the coordinator's timeout names it. An optional one that never
    /// resolves yields no binding and the consumer becomes ready anyway.
    ///
    /// **After it has resolved**, losing a required contract exits the host
    /// once its re-resolve budget is spent; losing an optional one does not.
    /// The re-resolve is identical either way — the coordinates still have to
    /// be put right, and `Bindings::connect` still fails while they are wrong —
    /// but an optional dependency that never comes back leaves a degraded
    /// process rather than a dead one.
    ///
    /// The second half used to be missing, and its absence was the more
    /// dangerous kind of bug: a consumer that declared a dependency optional
    /// *because* it could carry on without it was killed sixty seconds after
    /// losing it, exactly like one that had declared it required.
    ///
    /// Defaults to `false` on the wire: omitting the field asks for the strict,
    /// safe reading rather than the permissive one.
    #[serde(default)]
    pub optional: bool,
}

impl Consumed {
    /// Declare a required dependency.
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        Self {
            contract: ContractId::new(name, version),
            optional: false,
        }
    }

    /// Declare an optional dependency — one whose absence must not stop the
    /// consumer from becoming ready.
    pub fn optional(name: impl Into<String>, version: u32) -> Self {
        Self {
            contract: ContractId::new(name, version),
            optional: true,
        }
    }
}

/// Identity of the machine a set of [`Endpoints`] was reported from — more
/// precisely, of the namespace in which its `uds` paths mean anything.
///
/// Equality is the **only** defined operation. The value is opaque: nothing
/// dials it, parses it, or resolves it, and it is deliberately not typed as a
/// DNS hostname, because a hostname is neither unique (two containers can both
/// be `localhost`) nor a statement about the filesystem (two containers on one
/// machine share a hostname but not a socket namespace). Two reports carrying
/// equal `HostId`s assert exactly one thing: *a path in one's `uds` list, dialled
/// from the other, reaches the process that published it.*
///
/// Whoever mints the value owes that assertion. A per-boot random token, or a
/// platform machine id combined with a container/namespace discriminator, both
/// satisfy it; the bare output of `gethostname` does not. This crate does not
/// mint one — that needs a runtime, and this crate has none.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostId(pub String);

impl HostId {
    /// Wrap an already-minted identity token.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The token, for logging and display.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One iroh dial coordinate: the shared endpoint ticket plus this service's
/// ALPN. The ticket identifies the process's endpoint; the ALPN selects which
/// mounted service on it is being dialled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrohDial {
    /// The node ticket, as produced when the iroh endpoint binds.
    pub ticket: String,
    /// The ALPN this service is mounted under.
    pub alpn: String,
}

/// How one producer can be reached, across every transport it is mounted on,
/// plus the identity of the host reporting them.
///
/// Where several coordinates are listed for one transport, the first is the
/// preferred one and the rest are fallbacks, tried in order.
///
/// # Why `host` is here from day one
///
/// A consumer prefers uds, then ws, then iroh — cheapest first. But a `uds`
/// entry is a bare path with **no machine identity**, so that preference is
/// only sound for a producer known to be local. A consumer on host B resolving
/// a producer on host A would either fail on a path that does not exist, or —
/// far worse — connect to an unrelated socket that happens to sit at the same
/// path on B, and succeed. The bug is silent and it is a mis-dial, not an
/// error.
///
/// So [`Endpoints`] carries the reporting host's [`HostId`], and a consumer
/// considers `uds` only when its own id equals this one. Multi-machine
/// operation is otherwise deferred, but this single field is not deferrable
/// with it: while nothing yet consumes the wire format, adding it is free;
/// afterwards it is a breaking wire change to every implementation, in every
/// language, at once.
///
/// There is deliberately no `Default`: an `Endpoints` with no locality is not a
/// meaningful value, and a defaulted empty `HostId` would compare *equal* to
/// another one — turning "locality unknown" into "same host", which is the
/// exact mis-dial the field exists to prevent. Build one with
/// [`Endpoints::new`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoints {
    /// Who reported these coordinates. See the type docs — this is the locality
    /// tag that makes `uds` safe to prefer.
    pub host: HostId,
    /// `ws://host:port/path` URLs.
    #[serde(default)]
    pub ws: Vec<String>,
    /// Unix domain socket paths. Meaningful **only** to a dialler whose own
    /// [`HostId`] equals `host`.
    #[serde(default)]
    pub uds: Vec<String>,
    /// iroh ticket + ALPN pairs. Always dialable, wherever the consumer runs.
    #[serde(default)]
    pub iroh: Vec<IrohDial>,
}

impl Endpoints {
    /// An empty set of coordinates tagged with the reporting host. Transports
    /// push their coordinates in as each one binds.
    pub fn new(host: HostId) -> Self {
        Self {
            host,
            ws: Vec::new(),
            uds: Vec::new(),
            iroh: Vec::new(),
        }
    }

    /// Whether a dialler on `who` may use the `uds` paths in this set.
    ///
    /// The whole point of the locality tag, spelled as one predicate so that no
    /// caller has to remember to write the comparison.
    pub fn uds_usable_from(&self, who: &HostId) -> bool {
        &self.host == who
    }

    /// Whether this set names no coordinate at all on any transport.
    ///
    /// A service whose every transport failed to bind is still registered —
    /// its `consumes` gates its host's readiness, and dropping the entry would
    /// silently drop that (§8) — but it is registered with an empty set, and
    /// **an empty set is not a producer anyone can be sent to**. Spelled here
    /// rather than as three `is_empty()` calls at each site, because the
    /// coordinator's selection policy, the host's own warning and any
    /// dashboard all have to agree on what "unreachable" means.
    pub fn is_empty(&self) -> bool {
        self.ws.is_empty() && self.uds.is_empty() && self.iroh.is_empty()
    }
}

/// A resolved dependency: the contract asked for, the producer chosen for it,
/// and where to dial that producer.
///
/// One is emitted per consumed contract as it resolves. `instance` is
/// informational — for logs, dashboards and a future pinning policy — not a
/// promise of stability: the interchangeability invariant (crate docs) means a
/// consumer that restarts may legitimately be handed a different instance for
/// the same contract, and must be correct either way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// The consumed contract this binding satisfies. Present because bindings
    /// arrive one at a time on a stream and must be correlated with what was
    /// asked for.
    pub contract: ContractId,
    /// The instance id of the producer that was chosen. Supplied by that
    /// service and stable across restarts of either side, so one service shows
    /// one identity in logs over time.
    pub instance: String,
    /// How to reach that producer.
    pub endpoints: Endpoints,
}
