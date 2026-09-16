//! The `coord.*` control protocol — op names and the argument / result shapes
//! a service exchanges with a coordinator.
//!
//! These shapes live here, beside the contract types, and **not** in the
//! coordinator crate, for the same reason the superseded
//! `peerline-manager-protocol` was split from `peerline-manager`: a host has
//! to call these ops without depending on
//! the coordinator's implementation, and a dashboard, a CLI or a service
//! written in another language has to read them without pulling in a registry.
//! The coordinator crate owns the registry, the handlers and the selection
//! policy; this module owns the wire.
//!
//! # The ops
//!
//! | Op | Kind | Purpose |
//! | --- | --- | --- |
//! | [`OP_REGISTER`] | unary | declare produces / consumes / endpoints |
//! | [`OP_READY`] | unary | mark serving; may answer [`ReadyResult::UnknownInstance`] |
//! | [`OP_HEARTBEAT`] | unary | liveness against the TTL; may answer [`HeartbeatResult::UnknownInstance`] |
//! | [`OP_UPDATE`] | unary | replace where this instance is reachable, mid-serve |
//! | [`OP_RESOLVE`] | unary | the ready producers of one contract |
//! | [`OP_AWAIT_READY`] | stream | one [`Binding`] per consumed contract as it resolves |
//! | [`OP_LIST`] | unary | the whole registry, for a dashboard or a CLI |
//! | [`OP_WATCH`] | stream | the whole registry, then every change to it |
//! | [`OP_BINDINGS`] | unary | the selections the coordinator actually made |
//!
//! # This module is mirrored by a manifest
//!
//! Every op, type and error code below is also declared in
//! `contracts/coord.v1.json`, which is what an implementor who is not writing
//! Rust reads. The manifest does not generate this file and this file is not
//! generated from it — see the crate docs for why that exception exists here
//! and nowhere else — so the two are kept in step by
//! `tests/coord_manifest_drift.rs`, which fails naming the type and the field
//! when they diverge. **Edit one and edit both.**
//!
//! The five types this module takes from the crate root travel in
//! `contracts/peerline.contract.v1.json`, a vocabulary, because the same
//! shapes appear on the probe path and in a dashboard with no coordinator
//! involved.
//!
//! **Registered and ready are distinct.** Registered means the coordinator
//! knows your coordinates; ready means you are serving *and* your own consumed
//! contracts resolved. Only ready producers are ever handed out, so readiness
//! is transitive and a cluster converges leaf-first with no separate "is the
//! cluster up" notion.
//!
//! # Why `ready` **and** `heartbeat` are requests
//!
//! A notification cannot report failure, and `ready` has a failure to report:
//! an instance whose entry expired while it was resolving. Dropping that
//! silently would leave a service **serving, believing it is ready, and
//! invisible to the coordinator forever** — with everything consuming it
//! hanging. So `ready` answers [`ReadyResult`], and on
//! [`ReadyResult::UnknownInstance`] the service re-registers and re-readies.
//!
//! `heartbeat` is a request for exactly the same reason, one step later in the
//! lifecycle. It was a notification at first, on the argument that nothing is
//! lost if one goes missing — the next is a second away. That argument is
//! about a *dropped* heartbeat, and it holds. The failure it misses is a
//! heartbeat that arrives and finds **no entry to refresh**: a coordinator
//! that expired an instance which had already readied — a stalled process, a
//! paused VM, a TTL set too tight — cannot say so, and the host goes on
//! heartbeating into a registry that has forgotten it, serving, believing
//! itself ready, and invisible to every consumer. `ready` is sent once, at
//! startup, so it cannot catch a loss that happens later; the heartbeat is the
//! only call still being made. So it answers [`HeartbeatResult`], and
//! [`HeartbeatResult::UnknownInstance`] sends the host down the same
//! re-register-and-re-ready path it already has. The cost is one response per
//! host per second, which is nothing.
//!
//! # Why there is an [`OP_UPDATE`], and why it replaces rather than patches
//!
//! [`OP_REGISTER`] is sent at startup and on reconnect, and until [`OP_UPDATE`]
//! existed those were the *only* moments a coordinator learned where an
//! instance could be reached. A host, however, keeps serving as long as
//! anything is listening: a ws accept loop that dies does not end the process,
//! it costs that one transport. The registered coordinates then described a ws
//! URL that no longer answered, and went on describing it for the life of the
//! process — so a consumer resolving that contract was handed an address that
//! could only fail to dial.
//!
//! `update` closes that. It carries the endpoint set the host is serving on
//! **right now**, and the coordinator replaces what it holds with it.
//!
//! **A full replacement, never a delta.** The host is the only party that knows
//! where it is listening, so a round trip that says "here is all of it" is
//! idempotent: sending it twice, or sending it after a reconnect that already
//! re-registered the same set, changes nothing. A delta would need the
//! coordinator to hold a merge rule that the host also holds — the same
//! objection [`WatchEvent::Upsert`] records for carrying whole instances rather
//! than patches — and the two would eventually disagree about a set that
//! nothing else can reconcile.
//!
//! The replacement covers the whole instance, not only the services named in
//! it: a registered service the args do not mention is left with **no**
//! coordinates. That is the conservative direction, and the choice is between
//! two failure modes rather than between a failure and safety. A sender that
//! under-reports makes a service unselectable, which is loud — it appears in
//! [`InstanceInfo::unreachable`], and §8 already defines exactly what a
//! coordinate-less service is. A merge that kept what it was not told about
//! would instead keep handing out the stale address, silently, which is the
//! bug this op exists to end.
//!
//! An update is also proof that the sender is alive, so a coordinator refreshes
//! liveness on it exactly as it does for any other op.
//!
//! # Errors: value or `RpcError`?
//!
//! Both appear, and the split is deliberate. A recoverable protocol state with
//! a defined next step is a **value** ([`ReadyResult::UnknownInstance`] — go
//! re-register). A condition the caller cannot make progress against by
//! retrying is an **error** ([`ERR_INSTANCE_EXISTS`] and
//! [`ERR_SINGLETON_OCCUPIED`], both misconfigurations a human has to undo;
//! [`ERR_UNRESOLVED`], a timeout that ends the process). Errors carry a
//! stable [`RpcError::code`] plus structured `data`, so a caller matches on
//! the code rather than on a message.
//!
//! [`OP_UPDATE`] answers an unregistered instance with
//! [`ERR_UNKNOWN_INSTANCE`] rather than with a value, and it is worth saying
//! why it sits on the other side of that line from `ready` and `heartbeat`.
//! Those two are **lifecycle transitions** whose answer a host branches its own
//! state machine on — each was very nearly a notification, and the whole
//! argument for making them requests was that the state they report must not be
//! droppable. `update` is a plain write against an entry that is supposed to
//! already exist, and "there is no such entry" is precisely what
//! [`ERR_UNKNOWN_INSTANCE`] already means for [`OP_AWAIT_READY`]. The recovery
//! is the same either way — re-register, and re-`ready` if already serving —
//! and a host reaching it from here has not learned anything new about its own
//! lifecycle, only that this particular write landed nowhere.
//!
//! [`Binding`]: crate::Binding
//! [`RpcError::code`]: https://docs.rs/peerline

use serde::{Deserialize, Serialize};

use crate::{Binding, Consumed, ContractId, Endpoints, Produced};

/// Where a coordinator listens unless configured otherwise.
///
/// The coordinator is uds-only (see the crate docs of `peerline-coordinator`),
/// so this is a socket path and not a URL. Both ends need the value — the
/// coordinator to bind it and every host to dial it — which is why it is here
/// rather than in either implementation.
pub const DEFAULT_SOCKET: &str = "/tmp/peerline-coord.sock";

/// Declare an instance's produced / consumed contracts and dial coordinates.
/// Unary, [`RegisterArgs`] -> [`RegisterAck`].
///
/// Ops are application-level names; the `$peerline/` prefix is reserved for
/// protocol ops and is deliberately not used here.
pub const OP_REGISTER: &str = "coord.register";
/// Mark a registered instance as serving. Unary, [`ReadyArgs`] ->
/// [`ReadyResult`].
pub const OP_READY: &str = "coord.ready";
/// Refresh an instance's last-seen time. Unary, [`Heartbeat`] ->
/// [`HeartbeatResult`]; send it at an interval well under the coordinator's
/// TTL.
///
/// A request rather than a notification so that a host expired while it was
/// already serving is *told*, rather than heartbeating forever into a registry
/// that has forgotten it. See the module docs.
pub const OP_HEARTBEAT: &str = "coord.heartbeat";
/// Replace where this instance is reachable, without re-declaring what it
/// produces. Unary, [`UpdateArgs`] -> [`UpdateAck`]; raises
/// [`ERR_UNKNOWN_INSTANCE`].
///
/// Sent mid-serve, whenever the set of transports a host is actually answering
/// on changes — which for a peerline host means a transport whose accept loop
/// died while the others carried on. Without it the coordinates published at
/// `Ready` were the only ones a coordinator ever had, and a dead transport's
/// URL stayed in the registry for the life of the process.
///
/// **It replaces; it does not patch, and it does not re-declare.** See the
/// module docs for why a full endpoint set is the only shape that stays
/// idempotent, and [`UpdateArgs`] for what the args do *not* carry.
pub const OP_UPDATE: &str = "coord.update";
/// The ready producers of one contract, best first. Unary, [`ResolveArgs`] ->
/// [`ResolveResult`].
pub const OP_RESOLVE: &str = "coord.resolve";
/// Wait for this instance's consumed contracts to resolve. A **stream** of
/// [`Binding`], one per contract; [`AwaitReadyArgs`] names the waiting
/// instance and its deadline.
pub const OP_AWAIT_READY: &str = "coord.await_ready";
/// Every registered instance and its state. Unary, any args -> [`Listing`].
pub const OP_LIST: &str = "coord.list";
/// Watch the registry. A **stream** of [`WatchEvent`]: one
/// [`WatchEvent::Snapshot`] carrying the whole registry, then one event per
/// change until the caller cancels. Any args.
///
/// This is what an observer should use instead of polling [`OP_LIST`] on a
/// timer. The dashboard and `peerline-mon --watch` both did that at 2s, which
/// is simultaneously too slow (a registration is visible up to two seconds
/// after it happened) and too fast (every observer pulls the entire registry
/// twenty-five times a minute whether or not anything changed).
///
/// # Why a stream and not the pubsub layer
///
/// peerline has a pubsub subsystem, and a registry feed looks like a topic. It
/// is deliberately not used here, for two reasons.
///
/// The decisive one is the consumer: the dashboard reaches this op through
/// `peerline-ts`, which has `callStream` and an async-iterable `StreamReceiver`
/// but **no pubsub runtime** — its pubsub support stops at the payload types
/// and `classify`. A pubsub-based watch would work from Rust and would have to
/// be hand-rolled in the browser, which is exactly the demultiplexing the
/// runtime exists to stop applications writing.
///
/// The second holds even where both are available. A watch is *per consumer,
/// correlated to the request that opened it* — it begins with a snapshot taken
/// for that caller and ends when that caller goes away — which is a stream's
/// shape, not a topic's. [`OP_AWAIT_READY`] is the same shape and the
/// precedent. Pubsub's shape is one producer fanning identical events out to
/// subscribers who each missed whatever preceded them; a snapshot would have to
/// be bolted on beside it.
///
/// # Cancellation
///
/// Dropping the receiver (or breaking out of the `for await`) cancels the
/// stream and the coordinator forgets the watcher — the same
/// cancel-on-drop the streaming layer gives every other stream op.
pub const OP_WATCH: &str = "coord.watch";
/// The producer this coordinator handed to each consumer, per contract.
/// Unary, any args -> [`BindingGraph`].
///
/// [`OP_LIST`] answers what every instance *declares*; this answers what was
/// actually chosen. The two are different questions, and only the second can
/// order a shutdown under redundancy (§10.2).
pub const OP_BINDINGS: &str = "coord.bindings";

/// [`OP_REGISTER`] was given an instance id that is already registered to a
/// **different set of services**.
///
/// Two live processes claiming one identity is a misconfiguration — retrying
/// cannot fix it — so it is an error rather than a value. Carries no `data`:
/// the id is already in the caller's hand.
///
/// # What is *not* a collision
///
/// Re-registering the same id with the same services, declaring the same
/// contracts, is the reconnect path and it succeeds: a service that
/// reconnects, or one restarted while its previous entry is still inside the
/// TTL, has to be able to say who it is again without being crash-looped until
/// the entry expires.
///
/// **Endpoints are not part of the comparison, and neither is `pid`.** Both
/// change on an ordinary restart — an ephemeral ws port resolves differently
/// every boot and an iroh ticket is minted at bind — while the instance id,
/// derived from the *configured* coordinates, does not. Comparing them made
/// every restart of such a host a fatal collision for one whole TTL. What
/// remains is what the id is a name for: which services this process mounts
/// and what they declare.
///
/// A re-registration clears readiness — it asserts where the service can be
/// reached, not that it is serving — so a service that is already serving
/// follows it with [`OP_READY`]. That is also what makes accepting changed
/// endpoints safe: the new coordinates are handed to nobody until the process
/// that reported them says it is serving.
pub const ERR_INSTANCE_EXISTS: i32 = 1001;
/// The named instance is not registered — it never registered, or its entry
/// has already expired.
///
/// Raised by [`OP_AWAIT_READY`] and by [`OP_UPDATE`]: both name an instance
/// the coordinator is expected to already hold, and neither has anything to do
/// with an entry that is not there.
///
/// [`OP_READY`] and [`OP_HEARTBEAT`] do **not** use this code — they answer a
/// value instead ([`ReadyResult`], [`HeartbeatResult`]), because they are
/// lifecycle transitions a host branches its own state machine on. The module
/// docs state where that line falls and why [`OP_UPDATE`] is on this side of
/// it. The **recovery is identical** wherever it comes from: re-register, and
/// re-send [`OP_READY`] if already serving.
pub const ERR_UNKNOWN_INSTANCE: i32 = 1002;
/// [`OP_AWAIT_READY`] hit its deadline with required contracts outstanding.
/// `data` is an [`Unresolved`].
pub const ERR_UNRESOLVED: i32 = 1003;
/// [`OP_REGISTER`] declared a producer of a contract that already has a live
/// producer and is declared [`Cardinality::Single`] by one of them. `data` is
/// a [`SingletonOccupied`].
///
/// # Why this is not [`ERR_INSTANCE_EXISTS`]
///
/// They are different mistakes with different fixes and the operator needs to
/// be told which one they made. `ERR_INSTANCE_EXISTS` says *two processes are
/// claiming one identity* — the ids collide and the deployments differ.
/// This says the ids are perfectly distinct and the **contract** is the thing
/// that admits only one holder: a second `coagent.storage/1` over its own
/// SQLite file is a config with one entry too many, or a stray process started
/// by hand, and the answer is to stop one of them rather than to rename it.
/// Sharing a code would put both under one message that is wrong for one of
/// them.
///
/// # Why an error rather than a value
///
/// §6's split: a recoverable state with a defined next step is a value
/// ([`ReadyResult::UnknownInstance`] — go re-register), and anything not
/// fixable by retrying is an error. A second producer of a singleton contract
/// is not fixable by retrying. It is fixed by a human removing one of the two,
/// and until then the process that was refused must not go on believing it is
/// part of the fleet — so a host treats this as fatal and exits, which is the
/// loud failure the whole enum exists to convert a silent one into.
///
/// # What is *not* occupied
///
/// The **same** instance re-registering. A service that reconnects, or that is
/// restarted while its previous entry is still inside the TTL, re-registers
/// its own id and must refresh rather than be refused as its own rival — the
/// same reconnect path [`ERR_INSTANCE_EXISTS`] is careful about, for the same
/// reason. And an incumbent whose registration has **expired** on the TTL no
/// longer holds anything: a coordinator sweeps before it checks, so a dead
/// singleton never blocks its own replacement.
///
/// [`Cardinality::Single`]: crate::Cardinality::Single
pub const ERR_SINGLETON_OCCUPIED: i32 = 1004;

/// One mounted service: its name, where it is reachable, and the contracts it
/// declares.
///
/// **Endpoints are per mounted service, not per contract.** A service's
/// coordinates are a property of where it is mounted, not of what it promises,
/// so a process producing two contracts from one mount reports one entry with
/// both — and a [`Binding`] copies the *producing service's* endpoints into
/// the reply.
///
/// `consumes` is carried per service because that is where it is declared, but
/// a coordinator only ever uses the union across an instance's services:
/// readiness is per host — one process, one registration, one lifecycle, and
/// the process is what a supervisor restarts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEntry {
    /// The service's stable name, as it appears in logs. Not an identity: use
    /// the instance id for that.
    pub service: String,
    /// How to reach this service, on every transport that actually bound.
    pub endpoints: Endpoints,
    /// The contracts this service implements.
    #[serde(default)]
    pub produces: Vec<Produced>,
    /// The contracts this service depends on.
    #[serde(default)]
    pub consumes: Vec<Consumed>,
}

impl ServiceEntry {
    /// A service entry declaring nothing yet; push into `produces` /
    /// `consumes`, or use the struct literal.
    pub fn new(service: impl Into<String>, endpoints: Endpoints) -> Self {
        Self {
            service: service.into(),
            endpoints,
            produces: Vec::new(),
            consumes: Vec::new(),
        }
    }
}

/// Args for [`OP_REGISTER`].
///
/// **The instance id is supplied by the service, not minted by the
/// coordinator.** It is stable — derived from the service name plus its mount
/// coordinates — and identical across restarts of either side, so one service
/// shows one identity in logs, in a dashboard and in a CLI over time; a future
/// pinning policy has a name to pin to; and a collision means two processes are
/// claiming one identity, which is a misconfiguration worth rejecting here
/// rather than discovering later ([`ERR_INSTANCE_EXISTS`]).
///
/// It is spelled as a `String` rather than a newtype to match
/// [`Binding::instance`], which is the same value seen from the other side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterArgs {
    /// This instance's stable, service-chosen identity.
    pub instance: String,
    /// The process id, for operators reading a dashboard. `0` when unknown —
    /// nothing dispatches on it.
    #[serde(default)]
    pub pid: u32,
    /// Every service this process mounts.
    #[serde(default)]
    pub services: Vec<ServiceEntry>,
}

/// Reply to [`OP_REGISTER`] — the accepted id, echoed back.
///
/// It echoes rather than assigns: a caller that sees its own id come back has
/// confirmation the coordinator holds the identity it thinks it does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterAck {
    /// The registered instance id — always equal to [`RegisterArgs::instance`].
    pub id: String,
}

/// Args for [`OP_HEARTBEAT`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// Which instance is still alive.
    pub instance: String,
}

/// Reply to [`OP_HEARTBEAT`].
///
/// Spelled as its own type rather than reusing [`ReadyResult`] because the two
/// answer different questions — "you are now marked serving" and "you are
/// still known here" — and a shared type would make a future value on one an
/// unannounced value on the other. The shape is identical on the wire, which
/// is what lets a caller handle both with the same arm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HeartbeatResult {
    /// The instance is registered; its last-seen time has been refreshed.
    Ok,
    /// No such instance: its entry expired, or the coordinator restarted since
    /// it registered. Re-register, and re-send [`OP_READY`] if already
    /// serving. Do **not** re-run [`OP_AWAIT_READY`] — that is a one-shot
    /// startup step and its bindings are already in hand.
    UnknownInstance,
}

/// Args for [`OP_READY`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyArgs {
    /// Which instance is now serving.
    pub instance: String,
}

/// Reply to [`OP_READY`].
///
/// [`Self::UnknownInstance`] is a value rather than an [`ERR_UNKNOWN_INSTANCE`]
/// error because it names a recoverable state with one defined next step —
/// re-register, then re-`ready` — and a caller should not have to match an
/// error code to drive its own state machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReadyResult {
    /// The instance is registered and is now marked ready.
    Ok,
    /// No such instance: it never registered, or its entry expired while it
    /// was resolving. Re-register and re-send [`OP_READY`]; do **not** re-run
    /// [`OP_AWAIT_READY`], which is a one-shot startup step.
    UnknownInstance,
}

/// One service's current coordinates, as [`OP_UPDATE`] reports them.
///
/// # Why this is not a [`ServiceEntry`]
///
/// A `ServiceEntry` carries `produces` and `consumes` as well as `endpoints`,
/// and both of those default to empty. An update built out of one would
/// therefore be unable to say "my coordinates changed" without also appearing
/// to say something about the contracts I declare — and since the empty list is
/// both the default and a meaningful value, a coordinator could not tell an
/// update that omitted them from one that withdrew them. The declarations are
/// [`OP_REGISTER`]'s to state and nothing else's to touch, so the shape that
/// carries an update simply has no field for them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEndpoints {
    /// Which mounted service these coordinates belong to, spelled exactly as
    /// [`ServiceEntry::service`] spelled it at registration. A name the
    /// registration did not declare is not an error and is ignored: it names
    /// nothing the coordinator holds.
    pub service: String,
    /// Where that service is reachable **now**, on every transport that is
    /// still answering. Empty is a legal and meaningful value: it says this
    /// service can no longer be dialled on anything, which is exactly what §8
    /// already spells for a service whose every transport failed to bind, and
    /// it makes the service unselectable rather than removing it.
    pub endpoints: Endpoints,
}

/// Args for [`OP_UPDATE`] — where this instance is reachable now, in full.
///
/// **This is the whole set, not the part that changed.** The coordinator
/// replaces what it holds: a registered service absent from `services` is left
/// with no coordinates at all. See the module docs for why a replacement is the
/// only shape that is safe to send twice, and why under-reporting is the
/// failure this trades for.
///
/// What it deliberately does not carry: `pid`, `produces`, `consumes`, or
/// anything else [`OP_REGISTER`] established. An update is not a
/// re-registration — it does not clear readiness, it cannot collide
/// ([`ERR_INSTANCE_EXISTS`]), and it cannot be refused for cardinality
/// ([`ERR_SINGLETON_OCCUPIED`]), because it changes nothing any of those three
/// are about. A host that really has changed what it declares has restarted,
/// and a restart re-registers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateArgs {
    /// The instance whose coordinates these are, as registered.
    pub instance: String,
    /// Every service this process mounts, with the coordinates it is serving
    /// on right now.
    ///
    /// An empty list is legal and says the whole instance is unreachable —
    /// every transport it had is gone, and it is still running. It is not a
    /// deregistration: the entry stays, its `consumes` still gate its own
    /// readiness, and the TTL is still what eventually removes it.
    #[serde(default)]
    pub services: Vec<ServiceEndpoints>,
}

/// Reply to [`OP_UPDATE`] — the instance whose coordinates were replaced,
/// echoed back.
///
/// It echoes for [`RegisterAck`]'s reason: a caller that sees its own id come
/// back has confirmation the coordinator applied the replacement to the entry
/// it meant. Spelled as its own type rather than a reuse of [`RegisterAck`]
/// for the reason [`HeartbeatResult`] is not [`ReadyResult`] — the two answer
/// different questions, and a field added to one would otherwise be an
/// unannounced field on the other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAck {
    /// The instance id that was updated — always equal to
    /// [`UpdateArgs::instance`].
    pub id: String,
}

/// Args for [`OP_RESOLVE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveArgs {
    /// The contract to resolve.
    pub contract: ContractId,
    /// Who is asking, if the caller is a registered instance.
    ///
    /// Optional because [`OP_RESOLVE`] is also how an operator's CLI asks
    /// "who could serve this?", and that caller has no identity to give. When
    /// it *is* given, the coordinator records the producer it handed back as
    /// this instance's current selection, which is what makes [`OP_BINDINGS`]
    /// track a consumer that re-resolved after a dropped connection (§9)
    /// rather than reporting the producer it was first given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

impl ResolveArgs {
    /// Ask anonymously: no selection is recorded. What a CLI sends.
    pub fn new(contract: ContractId) -> Self {
        Self {
            contract,
            instance: None,
        }
    }

    /// Ask as a registered instance, so the answer is recorded as this
    /// instance's selection.
    pub fn by(instance: impl Into<String>, contract: ContractId) -> Self {
        Self {
            contract,
            instance: Some(instance.into()),
        }
    }
}

/// Reply to [`OP_RESOLVE`] — every **ready** producer of the contract.
///
/// Ordered by the coordinator's selection policy: `bindings[0]` is the
/// coordinator's choice and the rest are alternates, in preference order. A
/// consumer re-resolving after a dropped connection normally takes the first
/// and only looks further if it cannot dial it.
///
/// Empty means no ready producer — which is not an error: a producer may
/// simply not have finished starting.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResolveResult {
    /// Ready producers, best first.
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

/// Args for [`OP_AWAIT_READY`].
///
/// **What is waited for is not re-declared here.** [`OP_REGISTER`] already
/// established this instance's `consumes`; repeating it would put two sources
/// of truth for one fact on adjacent calls. The args carry only what
/// identifies the wait: who is waiting, and for how long.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitReadyArgs {
    /// The waiting instance, as registered.
    pub instance: String,
    /// How long to wait before failing with [`ERR_UNRESOLVED`]. There is no
    /// "wait forever": a cluster's characteristic failure is silence, and a
    /// deadline is what converts silence into a diagnosis.
    pub timeout_ms: u64,
}

/// `data` on an [`ERR_UNRESOLVED`] error.
///
/// The whole point of the error: name what did not resolve, and — if the
/// contract graph suggests one — the cycle that may be why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unresolved {
    /// The instance that gave up waiting.
    pub instance: String,
    /// How long it waited, echoing [`AwaitReadyArgs::timeout_ms`].
    pub waited_ms: u64,
    /// The required contracts that had no ready producer at the deadline.
    /// Optional contracts never appear: they do not gate readiness.
    pub unresolved: Vec<ContractId>,
    /// A cycle in the registered contract graph that passes through one of
    /// [`Self::unresolved`] — as a **path**, with the head repeated at the
    /// end, so it renders directly: `storage/1 -> index/1 -> storage/1`.
    /// Empty when no cycle touches the outstanding contracts.
    ///
    /// **Advisory, and an over-approximation.** Collapsing instances into
    /// contracts invents cycles that no actual deployment has — with two
    /// interchangeable producers of one contract, one of which consumes a
    /// second contract produced by a consumer of the first, the contract graph
    /// has a cycle while every instance still resolves. So this is a hint
    /// printed in an error, never a reason to reject anything.
    #[serde(default)]
    pub cycle: Vec<ContractId>,
}

/// `data` on an [`ERR_SINGLETON_OCCUPIED`] error.
///
/// Names both ends, because neither is in the caller's hand: the refused
/// process knows what it declared but not who already holds the contract, and
/// the operator reading the message needs the incumbent's instance id to go
/// and stop it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SingletonOccupied {
    /// The contract that admits only one live producer.
    pub contract: ContractId,
    /// The instance that was refused — the one that just called
    /// [`OP_REGISTER`].
    pub instance: String,
    /// The instance already producing the contract. Still live: it heartbeated
    /// within the TTL, or it would have been swept before the check ran.
    pub holder: String,
    /// Whether the two producers disagreed about the cardinality — one of them
    /// declared `single` and the other `any`.
    ///
    /// The refusal is the same either way (the strict reading wins), but the
    /// *diagnosis* is not: with both declaring `single`, somebody started two
    /// of a service that admits one; with a disagreement, the two binaries
    /// hold different beliefs about the contract and one of them needs fixing
    /// whatever else is done about the duplicate.
    #[serde(default)]
    pub disagreed: bool,
}

/// Reply to [`OP_LIST`] — the whole registry, as the coordinator currently
/// holds it (expired instances already swept), **and the coordinator itself**.
///
/// # Why the coordinator describes itself here
///
/// Two facts about the coordinator are needed to read its own answer, and
/// nothing else in the reply carries them.
///
/// [`InstanceInfo::last_seen_ms`] is meaningless without the TTL to compare it
/// against: `9.4s` since the last heartbeat is perfectly healthy at a two
/// minute TTL and within a rounding error of fatal at ten seconds. A reader
/// that does not know which cannot colour a row, and an operator that does not
/// know cannot decide whether to worry.
///
/// And [`Self::address`] is the only thing that says *which* coordinator
/// answered. The superseded manager got this for free by registering with
/// itself; a coordinator deliberately does not (§10.7), so an operator with
/// two of them running — a stale one on the default socket, the real one
/// somewhere else — otherwise has no way to tell from the output which they
/// are looking at.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Listing {
    /// How long an instance survives without a heartbeat, in milliseconds.
    /// `0` from a coordinator too old to report it.
    #[serde(default)]
    pub ttl_ms: u64,
    /// Where this coordinator is listening, as an operator would type it —
    /// normally a Unix socket path. Empty when it was not told its own
    /// address, which is a coordinator embedded in another process rather
    /// than a misconfiguration.
    #[serde(default)]
    pub address: String,
    /// Every live instance, ordered by instance id — the same order selection
    /// uses, so a reader sees the registry the way the coordinator does.
    #[serde(default)]
    pub instances: Vec<InstanceInfo>,
}

/// One registered instance, as [`OP_LIST`] reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    /// Its stable, service-chosen id.
    pub instance: String,
    /// Its process id, or `0` if it did not report one.
    pub pid: u32,
    /// Whether it has sent [`OP_READY`]. Only ready instances are handed out.
    pub ready: bool,
    /// Milliseconds since its last heartbeat (or registration).
    pub last_seen_ms: u64,
    /// Everything it mounts.
    pub services: Vec<ServiceEntry>,
    /// The required contracts it is parked on right now, if it is inside
    /// [`OP_AWAIT_READY`]. Empty for an instance that is not waiting.
    ///
    /// This is the field that answers the question a stuck cluster actually
    /// poses — *who is waiting for what* — without correlating log files.
    #[serde(default)]
    pub waiting_on: Vec<ContractId>,
    /// The names of this instance's services that reported **no coordinates on
    /// any transport**, and so cannot be dialled by anybody.
    ///
    /// A service whose every transport failed to bind is still registered
    /// (§8): dropping it would silently drop its `consumes` from the host's
    /// readiness union, which is worse than an unreachable service. But it is
    /// then a service that is running, may be `ready`, and can never be
    /// selected — and until this field existed that fact appeared **only in
    /// the producer's own log**, so an operator watching a consumer fail to
    /// resolve had nothing in the registry to point at.
    #[serde(default)]
    pub unreachable: Vec<String>,
}

/// One frame of the [`OP_WATCH`] stream.
///
/// # The shape, and why there are only three variants
///
/// A watch is a snapshot followed by deltas, and the registry is a map keyed by
/// instance id — so structurally there are exactly three things a frame can
/// say: *here is everything*, *here is one entry's current state*, *this entry
/// is gone*. A consumer that applies them to a map by id ends up with the same
/// [`Listing`] the coordinator holds, which is precisely what the 2s poll it
/// replaces used to fetch whole.
///
/// The *causes* an observer might care about — registered, became ready,
/// endpoints changed, started or stopped waiting — are deliberately **not**
/// variants. They are all "this instance's state changed", they all carry the
/// same payload, and a consumer that wants to distinguish them can: it is
/// holding the previous [`InstanceInfo`] for that id, so `!was.ready &&
/// is.ready` is "became ready" and needs no help from the wire. Spending a
/// variant per cause would fix today's list of interesting transitions into the
/// protocol, and every later one would be a breaking change to an internally
/// tagged enum. Three structural variants close the set instead.
///
/// Removal carries no reason for the same reason it needs none today: the
/// registry has exactly one way to lose an instance, the TTL sweep. If a
/// deliberate deregistration is ever added it arrives as an additional
/// `#[serde(default)]` field on [`Self::Remove`], not as a fourth variant.
///
/// # Ordering guarantees
///
/// [`Self::Snapshot`] is always the first frame and arrives exactly once. No
/// change can slip between the snapshot and the first delta: the coordinator
/// takes the snapshot and installs the watcher under a single lock acquisition,
/// the same discipline [`OP_AWAIT_READY`]'s seed-then-park rests on. Deltas
/// then arrive in the order the registry applied them.
///
/// A consumer may receive an [`Self::Upsert`] for an instance already in its
/// map (that is the normal case — it means the entry changed) and must treat it
/// as a replace rather than an insert. It may **not** receive a [`Self::Remove`]
/// for an id it has never seen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchEvent {
    /// The registry as it stood the moment this watch was installed. Always
    /// the first frame of the stream, and sent exactly once.
    Snapshot {
        /// The whole registry — the identical value [`OP_LIST`] would have
        /// returned at that instant, carrying the coordinator's own TTL and
        /// address as well as its instances.
        ///
        /// Reusing [`Listing`] rather than restating its fields is the point:
        /// the snapshot and the poll it replaces cannot drift into describing
        /// the registry differently, and a consumer switching from one to the
        /// other reuses its rendering unchanged.
        #[serde(default)]
        registry: Listing,
    },
    /// One instance's current state, in full. Sent when it registers, when it
    /// becomes ready, when it heartbeats, when its endpoints or services
    /// change, and when what it is waiting on changes.
    ///
    /// Always the complete [`InstanceInfo`], never a patch: a partial update
    /// would need the consumer to hold a merge rule that the coordinator also
    /// holds, and the two would eventually disagree.
    Upsert {
        /// The instance, exactly as [`OP_LIST`] would report it now.
        instance: InstanceInfo,
    },
    /// An instance has left the registry — its TTL elapsed with no heartbeat.
    ///
    /// This is the event polling could not deliver promptly and the reason the
    /// coordinator daemon runs a sweep timer while anyone is watching: expiry
    /// is generated by the sweep, the sweep is lazy, and on a fleet that has
    /// gone entirely silent there are no ops left to trigger one.
    Remove {
        /// The instance id that is gone. Drop it from your view.
        instance: String,
    },
}

/// Reply to [`OP_BINDINGS`] — every selection the coordinator has made and
/// still believes in.
///
/// # What a recorded selection is, and is not
///
/// It is **the coordinator's best knowledge, not a guarantee.** The
/// coordinator records the producer it handed to a consumer, at the moment it
/// handed it over — through [`OP_AWAIT_READY`] at startup or [`OP_RESOLVE`] on
/// a re-resolve. What the consumer did with it is not observable from here:
/// the data path never touches the coordinator (§3), so a consumer that
/// failed to dial the producer and fell back to an alternate in the same
/// [`ResolveResult`], or that is mid-reconnect right now, has an edge here
/// that is one step out of date. [`Edge::age_ms`] is how stale it might be.
///
/// It is still strictly better than the declared contracts [`OP_LIST`]
/// returns. With two ready producers of `storage/1`, declarations say only
/// that `fs` consumes it and orders `fs` before *both*; a recorded selection
/// names the one `fs` was given. A superset ordering is safe but coarse, and
/// under redundancy — the thing the selection policy exists to support — it
/// gets coarser exactly as the cluster gets more interesting.
///
/// An edge is dropped when either end leaves the registry: a selection
/// pointing at an expired producer describes nothing.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BindingGraph {
    /// Every recorded selection, ordered by consumer then contract — the same
    /// stable ordering [`Listing`] uses, and for the same reason.
    #[serde(default)]
    pub edges: Vec<Edge>,
}

/// One recorded selection: *this* consumer was given *that* producer for a
/// contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// The instance that asked — as registered.
    pub consumer: String,
    /// What it asked for.
    pub contract: ContractId,
    /// The instance it was given.
    pub producer: String,
    /// How long ago the selection was made, in milliseconds. A large age is
    /// not staleness by itself — a binding that has worked for a week is the
    /// normal case — but it does bound how recently the coordinator was in a
    /// position to know.
    pub age_ms: u64,
}
