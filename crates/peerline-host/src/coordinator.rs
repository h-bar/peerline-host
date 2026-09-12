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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use peerline::runtime::{Error, Peer, StreamReceiver};
use peerline_contract::coord::{
    AwaitReadyArgs, ERR_INSTANCE_EXISTS, ERR_UNKNOWN_INSTANCE, ERR_UNRESOLVED, Heartbeat,
    HeartbeatResult, OP_AWAIT_READY, OP_HEARTBEAT, OP_READY, OP_REGISTER, OP_RESOLVE, ReadyArgs,
    ReadyResult, RegisterAck, RegisterArgs, ResolveArgs, ResolveResult, ServiceEntry,
};
use peerline_contract::{Binding, ContractId};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::bindings::{Bindings, LostRx};

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
/// re-sends exactly the same registration, which is what makes it a *refresh*
/// rather than a collision.
pub(crate) struct CoordCtx {
    pub socket: PathBuf,
    pub instance: String,
    pub pid: u32,
    pub entries: Vec<ServiceEntry>,
    /// Whether anything is consumed at all. With nothing to wait for, the
    /// `AwaitReady` step is skipped rather than round-tripped.
    pub awaits: bool,
    pub await_timeout: Duration,
}

/// How a session ended.
enum End {
    /// The connection (or the coordinator) went away. Reconnect.
    Retry(String),
    /// Nothing a retry can fix. The host exits non-zero.
    Fatal(String),
}

/// Keep this host registered, heartbeating and ready for as long as it runs.
///
/// Returns only on a fatal condition, and returns the message the host exits
/// with; every recoverable failure is retried with exponential backoff.
pub(crate) async fn client(
    ctx: Arc<CoordCtx>,
    bindings_tx: oneshot::Sender<Vec<Binding>>,
    mut serving: watch::Receiver<bool>,
) -> String {
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
    let heartbeat_serving = serving.clone();
    let work = async {
        register(&peer, ctx).await?;
        // From here on the heartbeat runs beside everything else. Nothing
        // below may starve it: a service parked in `await_ready` for longer
        // than the TTL would otherwise be expired while perfectly healthy.
        tokio::select! {
            e = heartbeat(&peer, ctx, heartbeat_serving) => Err(e),
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
    Err(End::Fatal(format!(
        "peerline-host: coordinator kept answering UnknownInstance for `{}` \
         immediately after a successful register",
        ctx.instance
    )))
}

async fn register(peer: &Peer, ctx: &CoordCtx) -> Result<(), End> {
    let args = RegisterArgs {
        instance: ctx.instance.clone(),
        pid: ctx.pid,
        services: ctx.entries.clone(),
    };
    match peer.call::<_, RegisterAck>(OP_REGISTER, &args).await {
        Ok(ack) => {
            info!(instance = %ack.id, coordinator = %ctx.socket.display(),
                  "peerline-host: registered");
            Ok(())
        }
        Err(Error::Rpc(e)) if e.code == ERR_INSTANCE_EXISTS => Err(End::Fatal(format!(
            "peerline-host: instance id `{}` is already registered with different \
             coordinates — two processes are claiming one identity: {}",
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
                if let Err(end) = register(peer, ctx).await {
                    return end;
                }
                if *serving.borrow() {
                    let args = ReadyArgs {
                        instance: ctx.instance.clone(),
                    };
                    match peer.call::<_, ReadyResult>(OP_READY, &args).await {
                        Ok(ReadyResult::Ok) => {
                            info!(instance = %ctx.instance, "peerline-host: ready again")
                        }
                        // Expired again between those two calls. The next beat
                        // is a second away and takes the same path; looping
                        // here would be a tight retry against a coordinator
                        // that is evidently struggling.
                        Ok(ReadyResult::UnknownInstance) => warn!(
                            instance = %ctx.instance,
                            "peerline-host: expired again immediately after re-registering"
                        ),
                        Err(e) => return retry_or_fatal("coord.ready", e),
                    }
                }
            }
            Err(e) => return retry_or_fatal("coord.heartbeat", e),
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
                    return Err(End::Fatal(format!(
                        "peerline-host: dependencies never resolved: {}",
                        e.message
                    )));
                }
                Err(e) => return Err(retry_or_fatal("coord.await_ready", e)),
            }
        }
        Ok(out)
    };
    match tokio::time::timeout(ctx.await_timeout + Duration::from_secs(5), collect).await {
        Ok(r) => r,
        Err(_) => Err(End::Fatal(format!(
            "peerline-host: the coordinator at {} never answered coord.await_ready",
            ctx.socket.display()
        ))),
    }
}

/// A protocol-level error is the coordinator refusing something and will refuse
/// it again; a transport-level one is the connection, and reconnecting is the
/// answer.
fn retry_or_fatal(op: &str, e: Error) -> End {
    match e {
        Error::Rpc(e) => End::Fatal(format!("peerline-host: {op}: {} ({})", e.message, e.code)),
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
/// # Concurrent, and deduplicated
///
/// Each contract is re-resolved in its own task. Awaiting one inline would
/// make a second dependency lost a moment later wait out the *first* one's
/// entire budget before anyone even tried to resolve it — two independent
/// failures serialised into one, with the second's clock running the whole
/// time. Conversely a contract already being re-resolved ignores further
/// reports for it: a service that re-dials in a loop reports a failure every
/// time round, and each one would otherwise start a duplicate budget.
pub(crate) async fn watch_dependencies(
    mut lost: LostRx,
    bindings: Bindings,
    socket: PathBuf,
    instance: String,
    budget: Duration,
) -> String {
    let mut inflight: HashSet<ContractId> = HashSet::new();
    let mut tasks: JoinSet<(ContractId, Result<String, String>)> = JoinSet::new();
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
                        (contract, r)
                    });
                }
                // Every `Bindings` handle was dropped: no service can dial a
                // dependency any more, so nothing new can be reported. Any
                // re-resolve still running keeps its budget.
                None => break,
            },
            Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                match done {
                    Ok((contract, Ok(producer))) => {
                        inflight.remove(&contract);
                        info!(%contract, instance = %producer,
                              "peerline-host: dependency re-resolved");
                    }
                    Ok((contract, Err(why))) => match spent(&bindings, &contract, &why) {
                        Some(fatal) => return fatal,
                        // Optional: degraded, not dead. Forget the attempt so
                        // the next failed dial can start another one.
                        None => { inflight.remove(&contract); }
                    },
                    // The task cannot panic — `re_resolve` returns its errors —
                    // but a lost JoinError must not be read as success.
                    Err(e) => return format!("peerline-host: re-resolve task failed: {e}"),
                }
            }
        }
    }
    while let Some(done) = tasks.join_next().await {
        match done {
            Ok((_, Ok(_))) => {}
            Ok((contract, Err(why))) => {
                if let Some(fatal) = spent(&bindings, &contract, &why) {
                    return fatal;
                }
            }
            Err(e) => return format!("peerline-host: re-resolve task failed: {e}"),
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
fn spent(bindings: &Bindings, contract: &ContractId, why: &str) -> Option<String> {
    if bindings.optional(contract) {
        warn!(
            %contract, reason = %why,
            "peerline-host: optional dependency did not come back; continuing \
             without it. This host will keep re-resolving it whenever a dial \
             fails, and stays up meanwhile — that is what `optional` means."
        );
        return None;
    }
    Some(format!("peerline-host: `{contract}` {why}; exiting"))
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
    use super::*;
    use peerline_contract::{Consumed, Endpoints, HostId};

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
        assert!(err.contains("storage/1"), "{err}");
        assert!(err.contains("did not come back"), "{err}");
        assert!(err.contains("exiting"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
