//! The host's side of the `coord.*` protocol: register, heartbeat, wait for
//! dependencies, announce readiness — and keep doing all of it across a
//! coordinator restart.
//!
//! # The pipeline, and the one thing that is not sequential
//!
//! ```text
//! Bind -> Register -> AwaitReady -> Service::bind(deps) -> Serve -> Ready
//!             |
//!   heartbeat starts here, concurrent with everything to its right
//! ```
//!
//! Heartbeating from registration rather than after readiness is not a detail.
//! A strictly sequential pipeline leaves a service registered but *silent* for
//! the whole `AwaitReady` window, so a dependency slower than the coordinator's
//! TTL causes it to expire a service that is healthy and waiting — after which
//! its `ready` is answered `UnknownInstance` and, if that were a notification,
//! silently dropped. The heartbeat therefore runs beside every later step, and
//! `coord.ready` is a request whose failure is acted on.
//!
//! # Reconnect is not the same path as startup
//!
//! The registry is soft state: a restarted coordinator comes back empty. So on
//! every reconnect this client re-registers, and re-sends `coord.ready` if the
//! host is already serving — a re-registration deliberately clears readiness,
//! and without the second half the whole cluster would end up
//! registered-but-not-ready with nothing new able to resolve.
//!
//! It does **not** re-run `await_ready` and does **not** call `Service::bind`
//! again: both are one-shot startup steps, and a second `OnceLock::set` inside
//! a service would be a silent no-op anyway.
//!
//! # Where this host can be reached is not fixed at `Ready`
//!
//! A host keeps serving as long as anything is listening, so a transport whose
//! accept loop dies costs that transport rather than the process — and the
//! coordinates registered at startup then name an address that can only fail to
//! dial. The serving stage reports each such loss by republishing the set this
//! host is *currently* answering on, and [`updates`] turns every change into a
//! `coord.update`.
//!
//! Two consequences are worth stating, because both look like missing code:
//!
//! **There is no retry ladder for an update, and there must not be one.**
//! [`register`] reads the live endpoint set rather than a copy frozen at bind,
//! so a reconnect re-registers the coordinates this host is serving on *now* —
//! which is precisely the replacement an update would have made. A session that
//! cannot send an update is a session that is about to end, and the reconnect
//! at the other side of it carries the same fact. Queueing one would be a
//! second mechanism racing the one that already works.
//!
//! **An update does not re-assert readiness.** Losing a transport is not a
//! lifecycle event: this host was serving before it and is serving after it.
//! Only a *re-registration* clears readiness, and only because it can arrive
//! from a process that has not started serving yet — which is exactly why the
//! `UnknownInstance` recovery below re-`ready`s and the ordinary update path
//! does not.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use peerline::runtime::{Error, Peer, StreamReceiver};
use peerline_contract::coord::{
    AwaitReadyArgs, ERR_INSTANCE_EXISTS, ERR_UNKNOWN_INSTANCE, ERR_UNRESOLVED, Heartbeat,
    HeartbeatResult, OP_AWAIT_READY, OP_HEARTBEAT, OP_READY, OP_REGISTER, OP_RESOLVE, OP_UPDATE,
    ReadyArgs, ReadyResult, RegisterAck, RegisterArgs, ResolveArgs, ResolveResult,
    ServiceEndpoints, ServiceEntry, UpdateAck, UpdateArgs,
};
use peerline_contract::{Binding, ContractId, Endpoints};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::bindings::{Bindings, LostRx};
use crate::error::HostError;

/// How often the host heartbeats. An order of magnitude under the
/// coordinator's default TTL, which is what makes an expiry mean something.
const HEARTBEAT: Duration = Duration::from_secs(1);

/// The longest gap between reconnect attempts.
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// How long a coordinator session must last to count as healthy, resetting the
/// reconnect backoff.
///
/// Without it the backoff only ever grows: a host that reconnects, runs
/// happily for a week and then sees the coordinator restarted waits the full
/// [`RECONNECT_MAX`] before its first attempt, because the delay still carries
/// the state of a failure a week old. The same reasoning — and the same
/// value — as `peerline_cluster::Backoff::reset_after`, which the supervisor
/// applies to its children for the same reason.
const RECONNECT_RESET_AFTER: Duration = Duration::from_secs(10);

/// Everything the client needs to speak for this host. Built once; a reconnect
/// re-sends the same *identity*, which is what makes it a refresh rather than a
/// collision — and the coordinates this host is serving on at that moment,
/// which is what makes it also an update.
pub(crate) struct CoordCtx {
    pub socket: PathBuf,
    pub instance: String,
    pub pid: u32,
    /// What this host mounts and declares, in mount order.
    ///
    /// The `endpoints` on each entry are the ones that bound and are **not**
    /// read after construction: [`CoordCtx::services`] overlays the live set
    /// over them, so nothing here can send a coordinate this host has stopped
    /// answering on. They are kept because they are the initial value of that
    /// live set and because a `ServiceEntry` has nowhere to be without one.
    pub entries: Vec<ServiceEntry>,
    /// Where this host is reachable **now** — one entry per mounted service, in
    /// the same order as [`Self::entries`], republished by the serving stage
    /// whenever a transport stops answering.
    pub endpoints: watch::Receiver<Vec<Endpoints>>,
    /// Whether anything is consumed at all. With nothing to wait for, the
    /// `AwaitReady` step is skipped rather than round-tripped.
    pub awaits: bool,
    pub await_timeout: Duration,
}

impl CoordCtx {
    /// This host's services carrying the coordinates it is serving on right
    /// now — what [`OP_REGISTER`] sends, at startup and on every reconnect.
    ///
    /// The overlay is positional because the two vectors are built from one
    /// iteration over the same mount list, in `Host::with_coordinator`. A live
    /// set that is somehow shorter falls back to the entry's own endpoints
    /// rather than inventing an empty one: guessing "unreachable" from a
    /// bookkeeping mismatch would withdraw a service that is answering
    /// perfectly well, which is the one outcome worse than a stale coordinate.
    fn services(&self) -> Vec<ServiceEntry> {
        let live = self.endpoints.borrow();
        self.entries
            .iter()
            .enumerate()
            .map(|(i, entry)| ServiceEntry {
                endpoints: live
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| entry.endpoints.clone()),
                ..entry.clone()
            })
            .collect()
    }

    /// The same set, as [`OP_UPDATE`] carries it: names and coordinates, and
    /// nothing an update is not allowed to restate.
    fn update_args(&self) -> UpdateArgs {
        let live = self.endpoints.borrow();
        UpdateArgs {
            instance: self.instance.clone(),
            services: self
                .entries
                .iter()
                .enumerate()
                .map(|(i, entry)| ServiceEndpoints {
                    service: entry.service.clone(),
                    endpoints: live
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| entry.endpoints.clone()),
                })
                .collect(),
        }
    }
}

/// How a session ended.
enum End {
    /// The connection (or the coordinator) went away. Reconnect.
    ///
    /// Stays a string: it is written to one `debug!` and never leaves this
    /// module, because a retried failure is not something a caller ever sees.
    Retry(String),
    /// Nothing a retry can fix. The host exits non-zero.
    Fatal(HostError),
}

/// The message a fatal coordinator condition carries out of this module.
///
/// Written without the `peerline-host:` prefix every log line here has:
/// [`HostError`]'s own `Display` adds it once, and doing it twice was the
/// first thing the typed errors made visible.
fn fatal(message: impl Into<String>) -> End {
    End::Fatal(HostError::Coordinator(message.into()))
}

/// Keep this host registered, heartbeating and ready for as long as it runs.
///
/// Returns only on a fatal condition, and returns the message the host exits
/// with; every recoverable failure is retried with exponential backoff.
pub(crate) async fn client(
    ctx: Arc<CoordCtx>,
    bindings_tx: oneshot::Sender<Vec<Binding>>,
    mut serving: watch::Receiver<bool>,
) -> HostError {
    let mut bindings_tx = Some(bindings_tx);
    const INITIAL: Duration = Duration::from_millis(50);
    let mut backoff = INITIAL;
    loop {
        let started = Instant::now();
        match session(&ctx, &mut bindings_tx, &mut serving).await {
            Ok(()) => {}
            Err(End::Fatal(e)) => return e,
            Err(End::Retry(e)) => {
                debug!(coordinator = %ctx.socket.display(), error = %e,
                       "peerline-host: coordinator session ended; reconnecting");
            }
        }
        // A session that lasted is evidence the coordinator is fine and this
        // was one event, not a loop; the next failure starts again from the
        // bottom rather than from wherever the last one climbed to.
        if started.elapsed() >= RECONNECT_RESET_AFTER {
            backoff = INITIAL;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// One connection's worth of protocol: register, then heartbeat concurrently
/// with the remaining lifecycle steps for as long as the wire lives.
async fn session(
    ctx: &CoordCtx,
    bindings_tx: &mut Option<oneshot::Sender<Vec<Binding>>>,
    serving: &mut watch::Receiver<bool>,
) -> Result<(), End> {
    let (peer, driver) = peerline_transport_uds::connect(&ctx.socket)
        .await
        .map_err(|e| End::Retry(e.to_string()))?;

    // The heartbeat needs to know whether this host is already serving, so
    // that an `UnknownInstance` answer can re-register *and* re-ready. Cloned
    // rather than shared: `lifecycle` holds the receiver mutably to wait on it.
    // `updates` needs it for the same recovery, by the same route.
    let heartbeat_serving = serving.clone();
    let update_serving = serving.clone();

    // Marked as seen **before** the registration below, not after. A transport
    // that dies while `register` is in flight would otherwise be marked seen by
    // a registration that may or may not have included it, and the loss would
    // be reported to nobody. Taking the mark first means the worst case is one
    // update restating what the registration already said, which `coord.update`
    // is a full replacement precisely so that it can absorb.
    let mut update_endpoints = ctx.endpoints.clone();
    drop(update_endpoints.borrow_and_update());

    let work = async {
        register(&peer, ctx).await?;
        // From here on the heartbeat runs beside everything else. Nothing
        // below may starve it: a service parked in `await_ready` for longer
        // than the TTL would otherwise be expired while perfectly healthy.
        // `updates` joins it rather than being spawned, for the reason the
        // whole client is a future: dropping this host drops all three at once.
        tokio::select! {
            e = heartbeat(&peer, ctx, heartbeat_serving) => Err(e),
            e = updates(&peer, ctx, update_endpoints, update_serving) => Err(e),
            r = lifecycle(&peer, ctx, bindings_tx, serving) => r,
        }
    };
    tokio::pin!(work);

    // The driver is owned here, so dropping this future (host shutdown) closes
    // the connection and ends the heartbeat with it. There is no
    // deregistration op — going quiet is how an instance leaves — so the
    // coordinator expires the entry one TTL later.
    tokio::select! {
        () = driver => Err(End::Retry("coordinator connection closed".into())),
        r = &mut work => r,
    }
}

/// The steps to the right of `Register`, in order.
async fn lifecycle(
    peer: &Peer,
    ctx: &CoordCtx,
    bindings_tx: &mut Option<oneshot::Sender<Vec<Binding>>>,
    serving: &mut watch::Receiver<bool>,
) -> Result<(), End> {
    // AwaitReady — once, on the first successful session only. A reconnect
    // must not re-run it: it is a startup step, and `Service::bind` has
    // already been called with what it produced.
    if bindings_tx.is_some() {
        let resolved = if ctx.awaits {
            await_ready(peer, ctx).await?
        } else {
            Vec::new()
        };
        if let Some(tx) = bindings_tx.take() {
            let _ = tx.send(resolved);
        }
    }

    // The host binds its services and starts serving; then, and only then, is
    // it honest to say `ready`.
    while !*serving.borrow_and_update() {
        if serving.changed().await.is_err() {
            // The host is on its way down. Nothing left to do on this
            // connection, but stay parked so the heartbeat beside us keeps
            // running until the whole future is dropped.
            return std::future::pending().await;
        }
    }

    // `UnknownInstance` is a value, not an error, because it names a
    // recoverable state with one defined next step: re-register, then re-ready.
    for _ in 0..3 {
        match peer
            .call::<_, ReadyResult>(
                OP_READY,
                &ReadyArgs {
                    instance: ctx.instance.clone(),
                },
            )
            .await
        {
            Ok(ReadyResult::Ok) => {
                info!(instance = %ctx.instance, "peerline-host: ready");
                return std::future::pending().await;
            }
            Ok(ReadyResult::UnknownInstance) => {
                warn!(instance = %ctx.instance,
                      "peerline-host: coordinator lost our registration; re-registering");
                register(peer, ctx).await?;
            }
            Err(e) => return Err(retry_or_fatal("coord.ready", e)),
        }
    }
    Err(fatal(format!(
        "the coordinator kept answering UnknownInstance for `{}` immediately after \
         a successful register",
        ctx.instance
    )))
}

/// Declare this host: its identity, and where it is reachable **now**.
///
/// The coordinates come from the live set rather than from what bound, and that
/// is what makes a reconnect self-correcting. A host that lost its ws listener
/// while the coordinator was down re-registers without it, and the registry is
/// right again with no update needing to have survived the outage. Endpoints are
/// deliberately excluded from the coordinator's identity comparison (§6), so a
/// shrunken set refreshes the entry rather than colliding with it.
async fn register(peer: &Peer, ctx: &CoordCtx) -> Result<(), End> {
    let args = RegisterArgs {
        instance: ctx.instance.clone(),
        pid: ctx.pid,
        services: ctx.services(),
    };
    match peer.call::<_, RegisterAck>(OP_REGISTER, &args).await {
        Ok(ack) => {
            info!(instance = %ack.id, coordinator = %ctx.socket.display(),
                  "peerline-host: registered");
            Ok(())
        }
        Err(Error::Rpc(e)) if e.code == ERR_INSTANCE_EXISTS => Err(fatal(format!(
            "instance id `{}` is already registered with different coordinates — two \
             processes are claiming one identity: {}",
            ctx.instance, e.message
        ))),
        Err(e) => Err(retry_or_fatal("coord.register", e)),
    }
}

/// Liveness. Never returns while the connection is up and the registration
/// holds.
///
/// # Why the answer is read
///
/// A heartbeat is a *request*, and the reply is the only thing that can tell
/// this host it has been forgotten. `coord.ready` reports the same condition,
/// but it is sent once, at startup — so an instance expired **after** it
/// readied (a stalled process, a paused VM, a TTL set too tight, a coordinator
/// restarted without dropping this connection) has no other way to find out.
/// It would go on beating into a registry with no entry to refresh: serving,
/// believing itself ready, and handed to nobody.
///
/// `UnknownInstance` therefore takes the path this client already has —
/// re-register, and re-`ready` if the host is serving. `await_ready` is *not*
/// re-run: it is a one-shot startup step and its bindings are already in hand.
async fn heartbeat(peer: &Peer, ctx: &CoordCtx, serving: watch::Receiver<bool>) -> End {
    let beat = Heartbeat {
        instance: ctx.instance.clone(),
    };
    loop {
        tokio::time::sleep(HEARTBEAT).await;
        match peer.call::<_, HeartbeatResult>(OP_HEARTBEAT, &beat).await {
            Ok(HeartbeatResult::Ok) => {}
            Ok(HeartbeatResult::UnknownInstance) => {
                warn!(instance = %ctx.instance,
                      "peerline-host: the coordinator has forgotten this instance \
                       (expired, or it restarted); re-registering");
                if let Err(end) = recover(peer, ctx, &serving).await {
                    return end;
                }
            }
            Err(e) => return retry_or_fatal("coord.heartbeat", e),
        }
    }
}

/// The one recovery this client has: re-register, and re-`ready` if this host
/// is already serving.
///
/// Shared by [`heartbeat`] and [`updates`] because it is the same recovery from
/// the same condition — the coordinator no longer holds this instance — reached
/// by two routes. Two copies would eventually do two different things about one
/// state, which is the whole failure the single `UnknownInstance` value exists
/// to make impossible.
///
/// For an update it is also *the update*: [`register`] sends the coordinates
/// this host is serving on now, so there is nothing left to re-send afterwards.
///
/// `Ok(())` covers "expired again between those two calls" as well as success.
/// The next heartbeat is a second away and takes this same path; looping here
/// would be a tight retry against a coordinator that is evidently struggling.
async fn recover(peer: &Peer, ctx: &CoordCtx, serving: &watch::Receiver<bool>) -> Result<(), End> {
    register(peer, ctx).await?;
    if !*serving.borrow() {
        return Ok(());
    }
    let args = ReadyArgs {
        instance: ctx.instance.clone(),
    };
    match peer.call::<_, ReadyResult>(OP_READY, &args).await {
        Ok(ReadyResult::Ok) => {
            info!(instance = %ctx.instance, "peerline-host: ready again");
            Ok(())
        }
        Ok(ReadyResult::UnknownInstance) => {
            warn!(
                instance = %ctx.instance,
                "peerline-host: expired again immediately after re-registering"
            );
            Ok(())
        }
        Err(e) => Err(retry_or_fatal("coord.ready", e)),
    }
}

/// Tell the coordinator where this host is reachable, every time that changes.
///
/// Never returns while the connection is up: like [`heartbeat`], this runs
/// beside the lifecycle for the whole session.
///
/// # What a change is, and what it is not
///
/// The serving stage republishes the whole endpoint set whenever a transport
/// stops answering, and only when that actually removed a coordinate. So every
/// wake-up here has something to say, and what it says is a **full
/// replacement** — `coord.update` is defined that way on purpose, and the
/// consequence is that this needs no memory of what it sent last, no diff, and
/// no ordering guarantee beyond "the latest value wins", which is what a
/// `watch` gives for free.
///
/// # Failure is not retried here
///
/// A transport-level failure ends the session, and the reconnect re-registers
/// with the current coordinates — the same replacement this would have sent.
/// That is the entire retry story, and adding a ladder would put a second
/// mechanism in the way of the one that already covers it.
///
/// A protocol-level failure is *not* fatal, which is the one place this departs
/// from [`retry_or_fatal`]. `coord.update` declares exactly one error, and it is
/// handled above it; anything else is a coordinator that does not know the op —
/// an older one, or a read-only surface, neither of which retrying or exiting
/// improves. A host that is serving correctly on its remaining transports must
/// not die because the registry will not take a correction.
async fn updates(
    peer: &Peer,
    ctx: &CoordCtx,
    mut endpoints: watch::Receiver<Vec<Endpoints>>,
    serving: watch::Receiver<bool>,
) -> End {
    loop {
        if endpoints.changed().await.is_err() {
            // The serving stage dropped its sender: this host is on its way
            // down. Park, so the heartbeat beside us keeps running until the
            // whole future is dropped — the same reason `lifecycle` parks.
            return std::future::pending().await;
        }
        let args = ctx.update_args();
        let reachable = args
            .services
            .iter()
            .filter(|s| !s.endpoints.is_empty())
            .count();
        match peer.call::<_, UpdateAck>(OP_UPDATE, &args).await {
            Ok(ack) => info!(
                instance = %ack.id,
                services = args.services.len(),
                reachable,
                "peerline-host: a transport stopped; withdrew its coordinates"
            ),
            Err(Error::Rpc(e)) if e.code == ERR_UNKNOWN_INSTANCE => {
                warn!(instance = %ctx.instance,
                      "peerline-host: the coordinator has forgotten this instance \
                       (expired, or it restarted); re-registering with current coordinates");
                if let Err(end) = recover(peer, ctx, &serving).await {
                    return end;
                }
            }
            // Not fatal, deliberately — see this function's docs.
            Err(Error::Rpc(e)) => warn!(
                instance = %ctx.instance, error = %e.message, code = e.code,
                "peerline-host: the coordinator refused coord.update; it may be older than \
                 this host. Coordinates for a transport that has stopped are still \
                 advertised there, and the next reconnect re-registers the current set"
            ),
            Err(e) => return End::Retry(format!("coord.update: {e}")),
        }
    }
}

/// Park until every **required** contract has a ready producer, collecting a
/// binding for each (and for any optional contract that happened to resolve).
async fn await_ready(peer: &Peer, ctx: &CoordCtx) -> Result<Vec<Binding>, End> {
    let mut rx: StreamReceiver<Binding> = peer
        .call_stream(
            OP_AWAIT_READY,
            &AwaitReadyArgs {
                instance: ctx.instance.clone(),
                timeout_ms: ctx.await_timeout.as_millis() as u64,
            },
        )
        .map_err(|e| retry_or_fatal("coord.await_ready", e))?;

    info!(instance = %ctx.instance, timeout = ?ctx.await_timeout,
          "peerline-host: waiting for dependencies");

    // The coordinator owns the deadline — it is the side that can say *what*
    // did not resolve. This guard only covers a coordinator that answers
    // nothing at all, which would otherwise park a service forever.
    let collect = async {
        let mut out = Vec::new();
        while let Some(item) = rx.next().await {
            match item {
                Ok(item) => {
                    info!(contract = %item.data.contract, instance = %item.data.instance,
                          "peerline-host: dependency resolved");
                    out.push(item.data);
                }
                Err(Error::Rpc(e)) if e.code == ERR_UNKNOWN_INSTANCE => {
                    return Err(End::Retry(format!("coord.await_ready: {}", e.message)));
                }
                Err(Error::Rpc(e)) if e.code == ERR_UNRESOLVED => {
                    return Err(fatal(format!("dependencies never resolved: {}", e.message)));
                }
                Err(e) => return Err(retry_or_fatal("coord.await_ready", e)),
            }
        }
        Ok(out)
    };
    match tokio::time::timeout(ctx.await_timeout + Duration::from_secs(5), collect).await {
        Ok(r) => r,
        Err(_) => Err(fatal(format!(
            "the coordinator at {} never answered coord.await_ready",
            ctx.socket.display()
        ))),
    }
}

/// A protocol-level error is the coordinator refusing something and will refuse
/// it again; a transport-level one is the connection, and reconnecting is the
/// answer.
fn retry_or_fatal(op: &str, e: Error) -> End {
    match e {
        Error::Rpc(e) => fatal(format!("{op}: {} ({})", e.message, e.code)),
        other => End::Retry(format!("{op}: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Dependency loss — re-resolve with a budget
// ---------------------------------------------------------------------------

/// Watch for dependencies becoming unreachable and put each one back,
/// re-resolving through the coordinator with exponential backoff.
///
/// Returns only when a **required** contract has stayed unresolvable for the
/// whole budget, and returns the message the host exits with.
///
/// **Re-resolving rather than reconnecting is deliberate**: the restarted
/// producer may have different coordinates, or the coordinator may now prefer a
/// different producer entirely. And exiting only when the budget is spent is
/// just as deliberate — exiting on the first drop would make a routine restart
/// of a popular producer take down its whole downward closure.
///
/// # An optional contract is re-resolved, and never exits the host
///
/// `Consumed::optional` used to govern startup alone, so a consumer that
/// declared a dependency optional exactly because it could survive losing it
/// was nevertheless killed sixty seconds after losing it. Here an optional
/// contract runs the identical re-resolve on the identical schedule — the
/// coordinates must still be put right, and `Bindings::connect` still reports
/// `Err` while they are wrong — but a spent budget logs and gives up on *that
/// attempt* rather than on the process. The contract leaves `inflight`, so the
/// next failed dial starts a fresh budget: a producer that returns after ten
/// minutes is picked up, which is the whole point of degrading rather than
/// dying.
///
/// # An optional contract that had no producer at startup is still sought
///
/// `optional` means the host does not *wait* for it, and `coord.await_ready`
/// duly returns without a binding for one that has no producer. Nothing then
/// looked for it again, ever: [`Bindings::connect`] deliberately reports no
/// loss for a contract with no binding — there is nothing to re-resolve — so
/// the contract never entered this loop, and a producer that started thirty
/// seconds after its consumer was never picked up for the life of the process.
/// A consumer that wanted it had to poll `coord.resolve` by hand, which is the
/// workaround this exists to remove.
///
/// So every contract in [`Bindings::unbound_optional`] gets a **seeker** here:
/// the same `coord.resolve`, on a backoff that is capped rather than budgeted.
/// When one answers, the binding enters `Bindings` with a revision bump, and
/// the documented `revision -> connect -> closed -> refreshed` loop picks it up
/// with no new vocabulary — a service written against a producer that was there
/// from the start needs no change to work with one that arrives late.
///
/// ## Capped, not budgeted
///
/// A lost dependency has a deadline because *something has broken* and the
/// host has to decide. An optional contract with no producer has not broken:
/// no-producer is its documented steady state, and a deadline would only mean
/// giving up on the very case the seeker exists for. So it runs for the life
/// of the host at [`SEEK_MAX`], which is the honest cost — one targeted
/// `coord.resolve` per unbound optional contract per thirty seconds, over a
/// local unix socket.
///
/// ## Why this polls rather than riding `coord.watch`
///
/// `coord.watch` exists now (`peerline_contract::coord::OP_WATCH`) and is the
/// right answer for an *observer* — a dashboard pulling the whole registry
/// every two seconds is what it was added to replace. It is deliberately not
/// used here, and the reason is not that it would not fit: it is a stream on a
/// peer, and [`await_ready`] already shows a stream living perfectly well
/// inside this future-not-a-task client.
///
/// The reason is that it could not replace anything. A watch event says an
/// instance appeared; it does not say that instance is *ours* to bind.
/// Selection is the coordinator's, and `coord.resolve` is what records the
/// producer it picked against this consumer — without that call
/// `coord.bindings` goes on reporting whatever we were given at startup, which
/// is precisely the answer that stops being true the moment a late producer is
/// bound. So the resolve stays either way, and watch could only ever be a
/// wake-up hint in front of it.
///
/// For that hint we would pay: a second long-lived connection to the
/// coordinator, with its own reconnect ladder duplicating [`client`]'s, plus
/// this poll retained anyway as the fallback for the window where that
/// connection is down. Two mechanisms, one of them load-bearing, to save one
/// small query per thirty seconds against a socket on this machine — which is
/// nothing like the whole-registry-every-two-seconds that `OP_WATCH`'s own
/// documentation argues against.
///
/// **What would change the call**: a host that needs a late producer bound in
/// under a second, or enough unbound optional contracts that the steady-state
/// query rate stops being negligible. The place to put it then is the
/// registration session in [`client`] — it already holds a live peer and a
/// reconnect ladder — with the event doing nothing but kicking the resolve
/// that already exists. Not a second connection.
///
/// # Concurrent, and deduplicated
///
/// Each contract is re-resolved in its own task. Awaiting one inline would
/// make a second dependency lost a moment later wait out the *first* one's
/// entire budget before anyone even tried to resolve it — two independent
/// failures serialised into one, with the second's clock running the whole
/// time. Conversely a contract already being re-resolved ignores further
/// reports for it: a service that re-dials in a loop reports a failure every
/// time round, and each one would otherwise start a duplicate budget. A seeker
/// takes the same slot, so a contract that has just been bound by one does not
/// also start a re-resolve.
pub(crate) async fn watch_dependencies(
    mut lost: LostRx,
    bindings: Bindings,
    socket: PathBuf,
    instance: String,
    budget: Duration,
) -> HostError {
    let mut inflight: HashSet<ContractId> = HashSet::new();
    let mut tasks: JoinSet<(ContractId, Found)> = JoinSet::new();

    // The contracts that resolved to nothing at startup. Started before the
    // loop, because nothing will ever report them: they have no binding, so no
    // dial against them can fail in a way that arrives on `lost`.
    for contract in bindings.unbound_optional() {
        inflight.insert(contract.clone());
        info!(
            %contract,
            "peerline-host: optional dependency has no producer yet; watching for one"
        );
        let (socket, bindings, instance) = (socket.clone(), bindings.clone(), instance.clone());
        tasks.spawn(async move {
            let producer = seek(&socket, &contract, &bindings, &instance).await;
            (contract, Found::Appeared(producer))
        });
    }

    loop {
        tokio::select! {
            report = lost.recv() => match report {
                Some(contract) => {
                    if !inflight.insert(contract.clone()) {
                        continue;
                    }
                    warn!(%contract, "peerline-host: dependency unreachable; re-resolving");
                    let (socket, bindings, instance) =
                        (socket.clone(), bindings.clone(), instance.clone());
                    tasks.spawn(async move {
                        let r = re_resolve(&socket, &contract, &bindings, &instance, budget).await;
                        let found = match r {
                            Ok(producer) => Found::Resolved(producer),
                            Err(why) => Found::Spent(why),
                        };
                        (contract, found)
                    });
                }
                // Every `Bindings` handle was dropped: no service can dial a
                // dependency any more, so nothing new can be reported. Any
                // re-resolve still running keeps its budget.
                None => break,
            },
            Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                match done {
                    Ok((contract, Found::Resolved(producer))) => {
                        inflight.remove(&contract);
                        info!(%contract, instance = %producer,
                              "peerline-host: dependency re-resolved");
                    }
                    Ok((contract, Found::Appeared(producer))) => {
                        inflight.remove(&contract);
                        info!(%contract, instance = %producer,
                              "peerline-host: an optional dependency that had no producer \
                               at startup is now bound");
                    }
                    Ok((contract, Found::Spent(why))) => match spent(&bindings, &contract, &why) {
                        Some(fatal) => return fatal,
                        // Optional: degraded, not dead. Forget the attempt so
                        // the next failed dial can start another one.
                        None => { inflight.remove(&contract); }
                    },
                    // The task cannot panic — `re_resolve` returns its errors —
                    // but a lost JoinError must not be read as success.
                    Err(e) => {
                        return HostError::Other(format!("a re-resolve task failed: {e}"));
                    }
                }
            }
        }
    }
    while let Some(done) = tasks.join_next().await {
        match done {
            Ok((_, Found::Resolved(_) | Found::Appeared(_))) => {}
            Ok((contract, Found::Spent(why))) => {
                if let Some(fatal) = spent(&bindings, &contract, &why) {
                    return fatal;
                }
            }
            Err(e) => return HostError::Other(format!("a re-resolve task failed: {e}")),
        }
    }
    std::future::pending().await
}

/// What a spent budget means for `contract`: `Some(message)` is the host's exit
/// message, `None` is "optional, so carry on without it".
///
/// The two paths differ only here. Everything before it — the schedule, the
/// backoff, the `coord.resolve` calls, the `Err` every `connect` returns
/// meanwhile — is the same for both, because an optional dependency being
/// missing is not an excuse to stop looking for it.
fn spent(bindings: &Bindings, contract: &ContractId, why: &str) -> Option<HostError> {
    if bindings.optional(contract) {
        warn!(
            %contract, reason = %why,
            "peerline-host: optional dependency did not come back; continuing \
             without it. This host will keep re-resolving it whenever a dial \
             fails, and stays up meanwhile — that is what `optional` means."
        );
        return None;
    }
    Some(HostError::DependencyLost {
        contract: contract.clone(),
        reason: why.to_string(),
    })
}

/// How one contract's search ended.
///
/// Three outcomes rather than a `Result`, because "a producer appeared for a
/// contract that never had one" and "a producer came back after its connection
/// died" are both successes and read completely differently in a log — and the
/// distinction is exactly the fix: the first is the case that used to be
/// impossible.
enum Found {
    /// A lost dependency's coordinates were put right, at this instance.
    Resolved(String),
    /// An optional contract with no producer at startup now has one.
    Appeared(String),
    /// The re-resolve budget ran out. The reason, bare — whether it is fatal
    /// is [`spent`]'s call.
    Spent(String),
}

/// The longest a seeker waits between asking whether an optional contract has
/// a producer yet.
///
/// Larger than [`RECONNECT_MAX`] on purpose. That one caps a backoff inside a
/// budget of about a minute, so it is spent within a dozen attempts; this one
/// is the **steady-state** rate of a loop that runs for the life of the
/// process, and its cost is what an idle host pays forever. Thirty seconds is
/// a round trip on a local unix socket twice a minute per unbound optional
/// contract, against a latency nobody is waiting on — the contract was
/// declared optional precisely because the host works without it.
const SEEK_MAX: Duration = Duration::from_secs(30);

/// Keep asking the coordinator whether an optional contract has a producer
/// yet, for as long as the host runs, and bind the first one that appears.
///
/// No deadline, deliberately: see [`watch_dependencies`]. Returns the instance
/// that was bound, which is the only way this ends short of the task being
/// dropped with the host.
async fn seek(socket: &Path, contract: &ContractId, bindings: &Bindings, instance: &str) -> String {
    let mut backoff = Duration::from_millis(100);
    loop {
        match resolve_once(socket, contract, instance).await {
            Ok(Some(binding)) => {
                let producer = binding.instance.clone();
                // The revision bump is what makes this visible to a service:
                // `replace` on a contract that had no entry leaves it at
                // revision 1, so a `refreshed(name, version, 0)` parked since
                // startup wakes with a binding in hand.
                bindings.replace(binding);
                return producer;
            }
            // Both are the ordinary case and neither is worth a warning: a
            // contract with no producer is what `optional` describes, and a
            // coordinator that is down is the reconnect ladder's business.
            Ok(None) => {}
            Err(e) => debug!(%contract, error = %e,
                             "peerline-host: still looking for an optional producer"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(SEEK_MAX);
    }
}

/// Keep asking the coordinator for a producer until one answers or the budget
/// runs out. `Ok` is the instance now bound; `Err` is why it never came back,
/// as a bare reason — see [`spent`] for what is done with it.
async fn re_resolve(
    socket: &Path,
    contract: &ContractId,
    bindings: &Bindings,
    instance: &str,
    budget: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + budget;
    let mut backoff = Duration::from_millis(100);
    // Assigned on every path through the loop before it can be read; declared
    // out here only so the failure message can quote the last attempt.
    let mut last;
    loop {
        match resolve_once(socket, contract, instance).await {
            Ok(Some(binding)) => {
                let instance = binding.instance.clone();
                bindings.replace(binding);
                return Ok(instance);
            }
            Ok(None) => last = "no ready producer".into(),
            Err(e) => last = e,
        }
        let now = Instant::now();
        if now >= deadline {
            // The *reason* only. Whether it is fatal is `spent`'s call, not
            // this one's — an optional contract runs exactly this loop and
            // must not be handed a string that says the host is exiting.
            return Err(format!("did not come back within {budget:?} ({last})"));
        }
        tokio::time::sleep(backoff.min(deadline - now)).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// One `coord.resolve`, on its own short-lived connection.
///
/// Deliberately not sharing the registration session's peer: this runs
/// concurrently with it, and a re-resolve must keep working across the window
/// where that session is itself reconnecting.
///
/// The call names this instance, so the coordinator records the producer it
/// hands back as *our* current selection. Without that, `coord.bindings` would
/// keep reporting whatever we were given at startup — and a re-resolve is
/// exactly the moment that answer stops being true.
async fn resolve_once(
    socket: &Path,
    contract: &ContractId,
    instance: &str,
) -> Result<Option<Binding>, String> {
    let (peer, driver) = peerline_transport_uds::connect(socket).await?;
    let args = ResolveArgs::by(instance, contract.clone());
    let call = peer.call::<_, ResolveResult>(OP_RESOLVE, &args);
    tokio::pin!(call);
    let result = tokio::select! {
        () = driver => return Err("coordinator connection closed".into()),
        r = &mut call => r.map_err(|e| e.to_string())?,
    };
    Ok(result.bindings.into_iter().next())
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use peerline_contract::{Consumed, Endpoints, HostId};

    use super::*;
    use crate::serving::{LiveEndpoints, Serve, TransportServe, serve_all};

    /// A binding to a socket nobody is listening on, so the first `connect`
    /// fails and reports the loss — the same report an established connection
    /// makes when it drops.
    fn unreachable(dir: &Path) -> Binding {
        let mut endpoints = Endpoints::new(HostId::new("h"));
        endpoints
            .uds
            .push(dir.join("nobody-home.sock").display().to_string());
        Binding {
            contract: ContractId::new("storage", 1),
            instance: "gone".into(),
            endpoints,
        }
    }

    /// A path with no coordinator on it, so every re-resolve attempt fails and
    /// the budget is certain to be spent.
    fn no_coordinator(dir: &Path) -> PathBuf {
        dir.join("no-coordinator.sock")
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pl-optional-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir scratch");
        dir
    }

    /// The half of §9 that used to be missing.
    ///
    /// `optional` governed startup alone, so a consumer that declared a
    /// dependency optional *because* it could carry on without it was killed
    /// sixty seconds after losing it — with the same budget, the same
    /// re-resolve and the same exit as a required one. The loss is still
    /// reported and still re-resolved; what must not happen is the exit.
    #[tokio::test]
    async fn a_spent_budget_on_an_optional_contract_does_not_exit_the_host() {
        let dir = scratch("optional");
        let (bindings, lost) = Bindings::new(
            HostId::new("h"),
            &[Consumed::optional("storage", 1)],
            vec![unreachable(&dir)],
        );

        let watchdog = watch_dependencies(
            lost,
            bindings.clone(),
            no_coordinator(&dir),
            "me".into(),
            Duration::from_millis(200),
        );
        tokio::pin!(watchdog);

        // The caller still learns: `connect` is `Err` whether or not the
        // process survives it.
        assert!(bindings.connect("storage", 1).await.is_err());

        tokio::select! {
            e = &mut watchdog => panic!(
                "an optional dependency must not end the host: {e}"
            ),
            () = tokio::time::sleep(Duration::from_millis(900)) => {}
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A coordinator that answers `coord.resolve` — with nothing until
    /// `producer` is filled in, and with that binding afterwards.
    ///
    /// Small enough to be honest: `coord.resolve` is the only op a seeker
    /// calls, and standing up the real coordinator would mean a dependency
    /// pointing the wrong way down the arrow (`peerline-cluster` depends on
    /// this crate, never the reverse).
    fn fake_coordinator(
        socket: PathBuf,
        producer: Arc<std::sync::Mutex<Option<Binding>>>,
    ) -> tokio::task::JoinHandle<()> {
        use peerline::wire::RpcError;
        use peerline_contract::coord::{ResolveArgs, ResolveResult};

        tokio::spawn(async move {
            let _ = peerline_transport_uds::serve(&socket, move |_| {
                let producer = producer.clone();
                Ok(Arc::new(move |peer: &Peer| {
                    let producer = producer.clone();
                    peer.on_request(OP_RESOLVE, move |_: ResolveArgs| {
                        let producer = producer.clone();
                        async move {
                            let bindings = producer
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone()
                                .into_iter()
                                .collect();
                            Ok::<_, RpcError>(ResolveResult { bindings })
                        }
                    });
                }) as peerline_transport_uds::PeerHandler)
            })
            .await;
        })
    }

    /// The documented downstream workaround, removed.
    ///
    /// An optional contract with no producer at startup resolved to nothing
    /// and was then **never looked at again**: `connect` reports no loss for a
    /// contract with no binding, so it never entered the watchdog, so a
    /// producer that started a moment later was invisible for the life of the
    /// process. A consumer that wanted one had to hand-roll `coord.resolve`
    /// polling.
    ///
    /// It must now bind on its own, and — the half that makes it usable —
    /// wake a service parked in `refreshed`, which is the loop the docs have
    /// told services to write all along.
    #[tokio::test]
    async fn an_optional_producer_that_appears_after_startup_is_bound_and_refreshes() {
        let dir = scratch("late-optional");
        let sock = dir.join("coord.sock");

        // Nothing to resolve yet: this is the state the host becomes ready in.
        let producer = Arc::new(std::sync::Mutex::new(None));
        let coordinator = fake_coordinator(sock.clone(), producer.clone());

        // An optional contract, and `await_ready` produced no binding for it.
        let (bindings, lost) = Bindings::new(
            HostId::new("h"),
            &[Consumed::optional("storage", 1)],
            Vec::new(),
        );
        assert!(
            bindings.get("storage", 1).is_none(),
            "the premise: it resolved to nothing"
        );

        // A service doing exactly what `Bindings::refreshed` documents: take
        // the revision, fail to connect, and park until the host has put a
        // binding in place.
        let parked = {
            let bindings = bindings.clone();
            tokio::spawn(async move {
                let seen = bindings.revision("storage", 1);
                assert_eq!(seen, 0, "a contract that never resolved starts at zero");
                assert!(bindings.connect("storage", 1).await.is_err());
                let rev = bindings.refreshed("storage", 1, seen).await;
                (rev, bindings.get("storage", 1))
            })
        };

        let watchdog = watch_dependencies(
            lost,
            bindings.clone(),
            sock.clone(),
            "me".into(),
            Duration::from_millis(200),
        );
        tokio::pin!(watchdog);

        // The producer registers late — after the host was already ready and
        // serving, which is the whole scenario.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!parked.is_finished(), "nothing has appeared yet");
        *producer.lock().expect("producer slot") = Some(Binding {
            contract: ContractId::new("storage", 1),
            instance: "storage-late".into(),
            endpoints: Endpoints::new(HostId::new("h")),
        });

        let (rev, bound) = tokio::select! {
            e = &mut watchdog => panic!("an optional contract must not end the host: {e}"),
            r = parked => r.expect("the parked task"),
        };
        assert!(rev > 0, "the late bind bumps the revision");
        assert_eq!(
            bound.expect("bound at last").instance,
            "storage-late",
            "the producer that appeared is the one now bound"
        );

        coordinator.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // A transport dies, and the coordinator is told
    // -----------------------------------------------------------------------

    /// Everything a stub coordinator saw, so a test can assert on the calls
    /// rather than on a log line.
    #[derive(Default)]
    struct CoordLog {
        registers: Vec<RegisterArgs>,
        updates: Vec<UpdateArgs>,
        readies: usize,
    }

    impl CoordLog {
        fn read(log: &Arc<std::sync::Mutex<Self>>) -> std::sync::MutexGuard<'_, Self> {
            log.lock().unwrap_or_else(|e| e.into_inner())
        }
    }

    /// A coordinator that answers the registration lifecycle and records it.
    ///
    /// The same shape as [`fake_coordinator`] and for the same reason: standing
    /// up the real coordinator would point a dependency the wrong way down the
    /// arrow (`peerline-cluster` depends on this crate, never the reverse). The
    /// four ops here are the entire set this client sends on a session that
    /// consumes nothing.
    ///
    /// `refuse_first_update` makes the stub answer the first `coord.update`
    /// with `ERR_UNKNOWN_INSTANCE`, which is how the recovery path is reached
    /// without having to make a stub expire anything.
    fn recording_coordinator(
        socket: PathBuf,
        log: Arc<std::sync::Mutex<CoordLog>>,
        refuse_first_update: bool,
    ) -> tokio::task::JoinHandle<()> {
        use peerline::wire::RpcError;

        tokio::spawn(async move {
            let _ = peerline_transport_uds::serve(&socket, move |_| {
                let log = log.clone();
                Ok(Arc::new(move |peer: &Peer| {
                    let l = log.clone();
                    peer.on_request(OP_REGISTER, move |args: RegisterArgs| {
                        let l = l.clone();
                        async move {
                            let id = args.instance.clone();
                            CoordLog::read(&l).registers.push(args);
                            Ok::<_, RpcError>(RegisterAck { id })
                        }
                    });
                    let l = log.clone();
                    peer.on_request(OP_READY, move |_: ReadyArgs| {
                        let l = l.clone();
                        async move {
                            CoordLog::read(&l).readies += 1;
                            Ok::<_, RpcError>(ReadyResult::Ok)
                        }
                    });
                    peer.on_request(OP_HEARTBEAT, move |_: Heartbeat| async move {
                        Ok::<_, RpcError>(HeartbeatResult::Ok)
                    });
                    let l = log.clone();
                    peer.on_request(OP_UPDATE, move |args: UpdateArgs| {
                        let l = l.clone();
                        async move {
                            let id = args.instance.clone();
                            let first = {
                                let mut g = CoordLog::read(&l);
                                let first = g.updates.is_empty();
                                g.updates.push(args);
                                first
                            };
                            if refuse_first_update && first {
                                return Err(RpcError {
                                    code: ERR_UNKNOWN_INSTANCE,
                                    message: "unknown instance".into(),
                                    data: None,
                                });
                            }
                            Ok(UpdateAck { id })
                        }
                    });
                }) as peerline_transport_uds::PeerHandler)
            })
            .await;
        })
    }

    /// One service reachable on ws and uds — the premise, since losing one
    /// transport has to be distinguishable from losing everything.
    fn on_both() -> Endpoints {
        Endpoints {
            host: HostId::new("pl-test"),
            ws: vec!["ws://127.0.0.1:9000/storage".to_string()],
            uds: vec!["/tmp/storage.sock".to_string()],
            iroh: Vec::new(),
        }
    }

    fn ctx_for(sock: &Path, endpoints: watch::Receiver<Vec<Endpoints>>) -> Arc<CoordCtx> {
        let mut entry = ServiceEntry::new("storage", on_both());
        entry
            .produces
            .push(peerline_contract::Produced::new("storage", 1));
        Arc::new(CoordCtx {
            socket: sock.to_path_buf(),
            instance: "storage-0".into(),
            pid: 4242,
            entries: vec![entry],
            endpoints,
            // Nothing consumed, so `await_ready` is skipped and the session is
            // register -> ready with the heartbeat and the updates beside it.
            awaits: false,
            await_timeout: Duration::from_secs(5),
        })
    }

    /// A ws accept loop that runs until it is told to die, the way a listener
    /// that is taken away mid-serve does.
    fn ws_until(killed: oneshot::Receiver<()>) -> Serve {
        Box::pin(async move {
            let _ = killed.await;
            Err(HostError::Serve("ws serve: listener gone".into()))
        })
    }

    /// The uds loop, which never stops — so the host keeps serving and
    /// `serve_all` never resolves, which is the whole scenario.
    fn uds_forever() -> Serve {
        Box::pin(std::future::pending::<Result<(), HostError>>())
    }

    /// **The staleness, closed end to end.** A transport dies while the host
    /// goes on serving, and the coordinator is told — with the *whole* current
    /// set, which is what `coord.update` is defined to carry.
    ///
    /// Before this the coordinates published at `Ready` were the only ones a
    /// coordinator ever had, so the dead ws URL stayed in the registry for the
    /// life of the process and every consumer resolving this contract was
    /// handed an address that could only fail to dial.
    #[tokio::test]
    async fn a_dead_transport_is_reported_to_the_coordinator_as_an_update() {
        let dir = scratch("coord-update");
        let sock = dir.join("coord.sock");
        let log = Arc::new(std::sync::Mutex::new(CoordLog::default()));
        let coordinator = recording_coordinator(sock.clone(), log.clone(), false);

        let (live, live_rx) = LiveEndpoints::watched(vec![on_both()]);
        let (bindings_tx, _bindings_rx) = oneshot::channel();
        let (serving_tx, serving_rx) = watch::channel(false);
        let mut client = Box::pin(client(ctx_for(&sock, live_rx), bindings_tx, serving_rx));

        let (kill_ws, ws_killed) = oneshot::channel::<()>();
        let mut serve = Box::pin(serve_all(
            vec![
                TransportServe::new("ws", |e| e.ws.clear(), ws_until(ws_killed)),
                TransportServe::new("uds", |e| e.uds.clear(), uds_forever()),
            ],
            live,
        ));

        // The host starts serving, so the client says `ready` — the state a
        // transport has to die *in* for any of this to be interesting.
        serving_tx
            .send(true)
            .expect("the client holds the receiver");
        drive(&mut client, &mut serve, "register and ready", || {
            let g = CoordLog::read(&log);
            (!g.registers.is_empty() && g.readies > 0).then_some(())
        })
        .await;

        // Nothing has changed yet, so nothing has been sent. Without this the
        // test would pass on a client that updated on a timer.
        assert!(
            CoordLog::read(&log).updates.is_empty(),
            "a host that has lost nothing sent an update"
        );

        kill_ws.send(()).expect("the ws loop is still running");
        let sent = drive(&mut client, &mut serve, "an update", || {
            CoordLog::read(&log).updates.first().cloned()
        })
        .await;

        assert_eq!(sent.instance, "storage-0");
        assert_eq!(sent.services.len(), 1, "one mounted service, one entry");
        assert_eq!(sent.services[0].service, "storage");
        assert!(
            sent.services[0].endpoints.ws.is_empty(),
            "the dead transport's URL was still advertised: {:?}",
            sent.services[0].endpoints.ws
        );
        assert_eq!(
            sent.services[0].endpoints.uds,
            on_both().uds,
            "an update carries the whole current set, not a diff — the transport that \
             is still answering has to be in it, or the coordinator withdraws that too"
        );
        assert_eq!(
            sent.services[0].endpoints.host,
            on_both().host,
            "the host token names a filesystem namespace, not a listener"
        );

        // One change, one update. A second would mean something is resending.
        assert_eq!(CoordLog::read(&log).updates.len(), 1);
        assert_eq!(
            CoordLog::read(&log).registers.len(),
            1,
            "an ordinary update must not re-register"
        );

        coordinator.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ERR_UNKNOWN_INSTANCE` takes the path this client already had, and the
    /// re-registration it sends carries the coordinates the host is serving on
    /// **now** — so the recovery is itself the update, and nothing has to be
    /// re-sent afterwards.
    ///
    /// That property is the reason there is no retry ladder for an update, so
    /// it is worth a test rather than a comment.
    #[tokio::test]
    async fn an_update_answered_unknown_instance_re_registers_with_the_current_endpoints() {
        let dir = scratch("coord-update-unknown");
        let sock = dir.join("coord.sock");
        let log = Arc::new(std::sync::Mutex::new(CoordLog::default()));
        let coordinator = recording_coordinator(sock.clone(), log.clone(), true);

        let (live, live_rx) = LiveEndpoints::watched(vec![on_both()]);
        let (bindings_tx, _bindings_rx) = oneshot::channel();
        let (serving_tx, serving_rx) = watch::channel(false);
        let mut client = Box::pin(client(ctx_for(&sock, live_rx), bindings_tx, serving_rx));

        let (kill_ws, ws_killed) = oneshot::channel::<()>();
        let mut serve = Box::pin(serve_all(
            vec![
                TransportServe::new("ws", |e| e.ws.clear(), ws_until(ws_killed)),
                TransportServe::new("uds", |e| e.uds.clear(), uds_forever()),
            ],
            live,
        ));

        serving_tx
            .send(true)
            .expect("the client holds the receiver");
        drive(&mut client, &mut serve, "the first register", || {
            let g = CoordLog::read(&log);
            (!g.registers.is_empty() && g.readies > 0).then_some(())
        })
        .await;

        kill_ws.send(()).expect("the ws loop is still running");
        let second = drive(&mut client, &mut serve, "a second register", || {
            CoordLog::read(&log).registers.get(1).cloned()
        })
        .await;

        assert!(
            second.services[0].endpoints.ws.is_empty(),
            "the re-registration put the dead transport's URL back: {:?}",
            second.services[0].endpoints.ws
        );
        assert_eq!(
            second.services[0].endpoints.uds,
            on_both().uds,
            "and dropped one that was still answering"
        );
        assert_eq!(
            second.services[0].produces.len(),
            1,
            "a re-registration still declares what this host produces"
        );

        // Readiness is re-asserted, because a re-registration clears it and
        // this host is serving. That is the half a bare re-register would miss.
        drive(&mut client, &mut serve, "ready again", || {
            (CoordLog::read(&log).readies >= 2).then_some(())
        })
        .await;

        coordinator.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Poll `done` while keeping the client and the serving future running, so
    /// that a test never waits on a future nobody is driving — and so that
    /// either of them resolving is an immediate, named failure rather than a
    /// timeout with nothing to say.
    async fn drive<T>(
        client: &mut (impl Future<Output = HostError> + Unpin),
        serve: &mut (impl Future<Output = Result<(), HostError>> + Unpin),
        what: &str,
        mut done: impl FnMut() -> Option<T>,
    ) -> T {
        for _ in 0..600 {
            if let Some(value) = done() {
                return value;
            }
            tokio::select! {
                e = &mut *client => panic!("the coordinator client stopped waiting for {what}: {e}"),
                r = &mut *serve => panic!("the host stopped serving waiting for {what}: {r:?}"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
        panic!("timed out waiting for {what}");
    }

    /// ...and the required case is untouched, which is what makes the change
    /// above a narrowing rather than a removal. Without this, "optional does
    /// not exit" would be indistinguishable from "nothing exits".
    #[tokio::test]
    async fn a_spent_budget_on_a_required_contract_still_exits_the_host() {
        let dir = scratch("required");
        let (bindings, lost) = Bindings::new(
            HostId::new("h"),
            &[Consumed::new("storage", 1)],
            vec![unreachable(&dir)],
        );

        let watchdog = watch_dependencies(
            lost,
            bindings.clone(),
            no_coordinator(&dir),
            "me".into(),
            Duration::from_millis(200),
        );
        tokio::pin!(watchdog);

        assert!(bindings.connect("storage", 1).await.is_err());

        let err = tokio::select! {
            e = &mut watchdog => e,
            () = tokio::time::sleep(Duration::from_secs(10)) => {
                panic!("a required dependency that never came back must exit the host")
            }
        };
        // The variant *is* the assertion now: a spent budget on a required
        // contract is `DependencyLost` and nothing else can be mistaken for
        // it. The reason is still checked, because which failure exhausted
        // the budget is the part an operator reads.
        let HostError::DependencyLost { contract, reason } = &err else {
            panic!("a required dependency that never came back exits the host: {err}")
        };
        assert_eq!(contract, &ContractId::new("storage", 1));
        assert!(reason.contains("did not come back"), "{reason}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
