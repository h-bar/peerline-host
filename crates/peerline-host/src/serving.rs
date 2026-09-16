//! Many accept loops, one future — and what a host does when one of them
//! stops.
//!
//! # The policy
//!
//! **A host keeps serving as long as anything is listening.** A transport
//! whose accept loop ends — with an error or without one — is dropped, logged,
//! and the remaining transports go on answering. Only when the *last* live
//! transport's loop ends does [`serve_all`] resolve, and it resolves with that
//! last loop's own result: an error if it failed, `Ok(())` if it simply ran
//! out of work.
//!
//! # Why it is not `try_join_all`
//!
//! The obvious combinator is the wrong one. `try_join_all` resolves on the
//! *first* error, so a ws accept loop that died took the uds one with it even
//! though the socket was still there, still admitted by the kernel's own
//! attestation, and still the transport everything local was reaching this
//! process on. That is a whole session lost to a door closing.
//!
//! Before this module the asymmetry was written into the code: iroh's serve
//! future swallowed its own error and parked on `pending()` forever, so an
//! iroh failure degraded, while ws and uds failures ended the run. The
//! difference was never a property of the transports — it was where the
//! `pending()` happened to be written. Downstream had the same shape
//! hand-rolled outside the host for the same reason (bind ws yourself, park
//! the failed arm forever, let uds decide the run's outcome), which is the
//! clearest evidence available that the host was missing it. So the special
//! case collapses into a rule that applies to every transport, and the
//! `pending()` moves here where it can say why it is there.
//!
//! # What "failure" means, per transport
//!
//! This is about an accept loop dying **mid-serve**, not about a bind. A
//! transport that never bound is already gone by the time anything here runs:
//! `bind_all` drops it, it contributes no coordinate to the endpoint report,
//! and the host fails only if nothing at all bound. Past that point each
//! transport reaches this module differently, and it is worth being exact
//! about how reachable each one is:
//!
//! * **ws** — `axum::serve` returns an error, which is what a TCP accept loop
//!   does when the listener itself is gone rather than when one connection
//!   failed.
//! * **uds** — a per-accept error is logged and retried by the transport, so
//!   its loop does not end while it has mounts. Its `serve` resolves `Ok(())`
//!   only when there were none. That is still "this transport is no longer
//!   listening", which is why the rule covers a clean end as well as a failure.
//! * **iroh** — the endpoint's accept loop ends when the endpoint closes,
//!   which on a laptop that changed networks is a routine event and not a
//!   reason to stop answering on a socket.
//!
//! # The endpoints for a dead transport are withdrawn
//!
//! A host builds its coordinates from what bound and publishes them at `Ready`.
//! When a transport's accept loop dies afterwards, the coordinates that named
//! it describe an address that can only fail to dial — so this module drops
//! them from the live set and says so, through a [`LiveEndpoints`] whose
//! receiver the coordinator client holds. The client turns each change into a
//! `coord.update`, and the registry stops handing the dead address out.
//!
//! **Which coordinates to drop is not decided here.** A transport knows what it
//! contributed and this combinator does not, so each accept loop arrives
//! carrying its own [`Withdraw`] — `|e| e.ws.clear()` for ws, and so on — built
//! where the transports are assembled and behind the same `cfg` they are. That
//! is what keeps `transport` a pure log label with nothing branching on it, and
//! what makes it impossible to add a transport whose death matches no arm.
//!
//! What this does **not** close is the last-transport case. When the final
//! accept loop ends, this future resolves and the host is on its way down with
//! it: the coordinator connection is dropped by the same shutdown, so there is
//! nobody left to send a withdrawal to. That instance leaves the registry the
//! way an instance always has — by falling silent, one TTL later. Withdrawing
//! into a connection that is being torn down would be a line of code that
//! looked like a guarantee and delivered nothing.
//!
//! See [`BoundHost::serve_until`](crate::BoundHost::serve_until) for what
//! remains of the staleness window once this is in place.

use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::watch;
use tracing::warn;

use crate::{Endpoints, HostError};

/// A transport's forever-running accept loop.
pub(crate) type Serve = BoxFuture<'static, Result<(), HostError>>;

/// How one transport's death is subtracted from a service's coordinates.
///
/// A plain `fn` pointer rather than a boxed closure: every one of these is a
/// non-capturing one-liner clearing one field, and there is exactly one per
/// transport for the whole host.
pub(crate) type Withdraw = fn(&mut Endpoints);

/// One transport's accept loop, labelled with the transport it belongs to and
/// carrying what its death costs.
///
/// The label exists for exactly one reason: the log line that says a transport
/// is gone has to name which one, and a bare future cannot be asked. It stays a
/// `&'static str` rather than an enum because **nothing branches on it** — the
/// one thing that has to vary per transport travels as [`Withdraw`] instead,
/// supplied by the assembly that already knows which transport this is and is
/// already behind that transport's `cfg`. A match on the label would have
/// needed a wildcard arm, and the failure it would absorb — a transport whose
/// coordinates are never withdrawn — is silent.
pub(crate) struct TransportServe {
    /// `"ws"`, `"uds"` or `"iroh"` — what a log line calls this transport.
    transport: &'static str,
    /// The coordinates this transport contributed, to be dropped when it stops.
    withdraw: Withdraw,
    /// The accept loop itself.
    serve: Serve,
}

/// The coordinates this host is **currently** serving on, and the channel that
/// republishes them when that changes.
///
/// # Why a `watch` rather than a queue of events
///
/// What the coordinator client has to send is a *full replacement*
/// (`coord.update` takes the whole set, deliberately — see
/// `peerline_contract::coord::UpdateArgs`), so the only thing it ever needs is
/// the latest value. That is exactly what a `watch` channel is: a slot with a
/// change notification, where a receiver that was busy during two changes wakes
/// once and reads the state both of them produced. A queue of transport deaths
/// would make the client responsible for replaying them in order into a set it
/// would have to maintain a second copy of — two copies of one fact, and a
/// merge rule on the receiving end, which is the shape the wire op refuses for
/// the same reason.
///
/// It also makes the client a **future rather than a task**, which the
/// coordinator client is on purpose: `changed()` is one more branch in a
/// `select!` beside the heartbeat, with no spawn, no handle to leak, and no
/// lifetime of its own to outlive the host.
pub(crate) struct LiveEndpoints {
    /// One entry per mounted service, in mount order — the same order
    /// `Host::endpoints` built and the coordinator client's `entries` are in.
    current: Vec<Endpoints>,
    /// `None` when nobody is listening for changes: a host with no coordinator
    /// has nothing to tell, and the withdrawals become bookkeeping nobody
    /// reads. Spelled as an `Option` rather than by sending into a channel with
    /// no receivers so that the no-coordinator path costs nothing at all.
    tx: Option<watch::Sender<Vec<Endpoints>>>,
}

impl LiveEndpoints {
    /// Track nothing: a host with no coordinator, or a test of the combinator
    /// itself.
    pub(crate) fn unwatched() -> Self {
        Self {
            current: Vec::new(),
            tx: None,
        }
    }

    /// Start from what bound, and hand back the receiver the coordinator client
    /// holds.
    ///
    /// The initial value is the set that was registered, so a client that reads
    /// before anything has died sees exactly what it published.
    pub(crate) fn watched(initial: Vec<Endpoints>) -> (Self, watch::Receiver<Vec<Endpoints>>) {
        let (tx, rx) = watch::channel(initial.clone());
        (
            Self {
                current: initial,
                tx: Some(tx),
            },
            rx,
        )
    }

    /// Drop everything one transport contributed, and republish if that
    /// actually removed something.
    ///
    /// The "if" is not an optimisation. A transport that bound but that no
    /// service mounted contributes no coordinate, so its accept loop ending
    /// changes nothing a consumer could observe — and waking the client to send
    /// an identical `coord.update` would put a round trip on the wire that says
    /// what the coordinator already believes.
    fn withdraw(&mut self, withdraw: Withdraw) {
        let Some(tx) = &self.tx else {
            return;
        };
        let before = self.current.clone();
        for endpoints in &mut self.current {
            withdraw(endpoints);
        }
        if before != self.current {
            // A closed channel means the client is already gone, which is a
            // host on its way down rather than anything to report.
            let _ = tx.send(self.current.clone());
        }
    }
}

// A host built with no transport compiled in has no accept loop to label, so
// nothing calls this — the same shape `Listening::into_serves` carries for the
// same reason. The policy is still compiled and still tested, because a
// transport being off is a build-time choice and not a different host.
#[cfg_attr(
    not(any(feature = "ws", feature = "uds", feature = "iroh")),
    allow(dead_code)
)]
impl TransportServe {
    /// Label an accept loop with the transport it belongs to, and with the
    /// coordinates its death withdraws.
    pub(crate) fn new(transport: &'static str, withdraw: Withdraw, serve: Serve) -> Self {
        Self {
            transport,
            withdraw,
            serve,
        }
    }
}

/// Serve every transport until the last one stops. See the module docs for
/// the policy and for what it costs.
///
/// An empty set resolves `Ok(())` immediately, which is the honest answer to
/// "serve nothing": there is nothing to wait for. No caller reaches it —
/// `bind_all` refuses a host on which nothing bound — and it is spelled out
/// rather than asserted because a future that parked forever on an empty set
/// would be a hang where this is a return.
pub(crate) async fn serve_all(
    serves: Vec<TransportServe>,
    mut endpoints: LiveEndpoints,
) -> Result<(), HostError> {
    type Ended = (&'static str, Withdraw, Result<(), HostError>);
    // `FuturesUnordered` rather than a hand-rolled `select`: the set shrinks by
    // one each time a loop ends, and this is the combinator that both polls all
    // of them and *forgets* the one that finished. The label and the withdrawal
    // ride along inside each future because the set hands back only the output.
    let mut live: FuturesUnordered<BoxFuture<'static, Ended>> = serves
        .into_iter()
        .map(|s| {
            let TransportServe {
                transport,
                withdraw,
                serve,
            } = s;
            Box::pin(async move { (transport, withdraw, serve.await) }) as BoxFuture<'static, Ended>
        })
        .collect();

    while let Some((transport, withdraw, result)) = live.next().await {
        // The completed future is already out of the set, so an empty set here
        // means the transport that just ended was the last one listening. Its
        // result is the host's: there is nothing left to degrade to, and
        // nothing to withdraw *to* either — see the module docs.
        if live.is_empty() {
            return result;
        }
        // Before the log line, so that an operator reading "continuing without
        // it" is reading about a host that has already stopped advertising it.
        endpoints.withdraw(withdraw);
        match result {
            Err(e) => warn!(
                transport,
                error = %e,
                "peerline-host: accept loop failed; continuing without it. Coordinates \
                 published for this transport have been withdrawn"
            ),
            // Not an error and still not nothing: whatever was reachable on
            // this transport is not reachable any more, and a coordinate for it
            // was published at Ready.
            Ok(()) => warn!(
                transport,
                "peerline-host: accept loop ended; continuing without it. Coordinates \
                 published for this transport have been withdrawn"
            ),
        }
    }

    // Only an empty input reaches here: the loop above returns on the last
    // element of a non-empty set.
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The policy, over synthetic accept loops.
    //!
    //! Synthetic rather than real transports, and that is the honest choice
    //! rather than a shortcut. A uds accept loop cannot be made to fail from
    //! outside the process — the transport logs and retries every per-accept
    //! error, by design — and inducing an `axum::serve` failure means reaching
    //! past the host to the listener it owns. What is actually under test is
    //! this combinator's arithmetic: who is dropped, who keeps running, and
    //! whose result comes out. A future is exactly as good a stand-in for an
    //! accept loop as the combinator is general.

    use std::time::Duration;

    use peerline_contract::HostId;
    use tokio::sync::{mpsc, oneshot};

    use super::*;

    /// The three withdrawals, spelled here exactly as `Listening::into_serves`
    /// spells them. Written out rather than imported because the real ones live
    /// behind three `cfg`s and a test must not stop compiling when a transport
    /// is switched off — what is under test is the arithmetic, not the wiring.
    fn drops_ws(e: &mut Endpoints) {
        e.ws.clear();
    }
    fn drops_uds(e: &mut Endpoints) {
        e.uds.clear();
    }
    fn drops_iroh(e: &mut Endpoints) {
        e.iroh.clear();
    }

    /// One service reachable on all three transports — the set a host of this
    /// shape would have registered at `Ready`.
    fn everywhere() -> Endpoints {
        Endpoints {
            host: HostId::new("pl-test"),
            ws: vec!["ws://127.0.0.1:9000/".to_string()],
            uds: vec!["/tmp/svc.sock".to_string()],
            iroh: vec![peerline_contract::IrohDial {
                ticket: "ticket".to_string(),
                alpn: "peerline/svc".to_string(),
            }],
        }
    }

    /// An accept loop that fails immediately, the way a listener that is gone
    /// would.
    fn dies(why: &str) -> Serve {
        let why = why.to_string();
        Box::pin(async move { Err(HostError::Serve(why)) })
    }

    /// An accept loop that runs forever without answering anything.
    fn lives() -> Serve {
        Box::pin(async { std::future::pending::<Result<(), HostError>>().await })
    }

    /// A stand-in for a transport that is still *answering*, not merely still
    /// pending: every message it is sent comes back with the number of
    /// messages it has handled. A host that had silently stopped polling this
    /// loop would be caught by the reply that never arrives.
    fn answers() -> (mpsc::UnboundedSender<oneshot::Sender<u32>>, Serve) {
        let (tx, mut rx) = mpsc::unbounded_channel::<oneshot::Sender<u32>>();
        let serve: Serve = Box::pin(async move {
            let mut answered = 0u32;
            while let Some(reply) = rx.recv().await {
                answered += 1;
                let _ = reply.send(answered);
            }
            // The sender was dropped: nothing can reach this loop again, which
            // is a loop that has ended rather than one that failed.
            Ok(())
        });
        (tx, serve)
    }

    /// **The property the whole module exists for.** ws dies; uds goes on
    /// answering; the host does not resolve. This is the hand-rolled shape
    /// downstream wrote outside the host — a dying ws accept loop is a door
    /// closing, not a session ending — made the host's own behaviour.
    #[tokio::test]
    async fn a_dead_accept_loop_leaves_the_others_answering() {
        let (ask, uds) = answers();
        let mut all = Box::pin(serve_all(
            vec![
                TransportServe::new("ws", drops_ws, dies("ws serve: listener gone")),
                TransportServe::new("uds", drops_uds, uds),
            ],
            LiveEndpoints::unwatched(),
        ));

        // Two round trips, each polling the combined future throughout: the ws
        // arm has already failed by the first one, so both answers are proof
        // that the failure was absorbed rather than propagated.
        for expected in 1..=2u32 {
            let (reply_tx, reply_rx) = oneshot::channel();
            ask.send(reply_tx).expect("the uds loop is still running");
            let answered = tokio::select! {
                r = &mut all => panic!("the host stopped serving: {r:?}"),
                a = reply_rx => a.expect("the uds loop answered"),
            };
            assert_eq!(answered, expected);
        }

        // And the host is still not resolved when nothing else happens.
        let stopped = tokio::time::timeout(Duration::from_millis(50), &mut all).await;
        assert!(stopped.is_err(), "the host resolved: {stopped:?}");
    }

    /// The other half of the rule: the failure is absorbed only while
    /// something else is listening. When the last loop goes, its error is the
    /// host's — which is what makes the exit code a restart signal instead of
    /// a process that sits there serving nothing.
    #[tokio::test]
    async fn the_last_transport_to_fail_ends_the_serve_with_its_error() {
        let (fail_uds, uds_fails) = oneshot::channel::<()>();
        let uds: Serve = Box::pin(async move {
            let _ = uds_fails.await;
            Err(HostError::Serve("uds serve: the socket was removed".into()))
        });
        let mut all = Box::pin(serve_all(
            vec![
                TransportServe::new("ws", drops_ws, dies("ws serve: listener gone")),
                TransportServe::new("uds", drops_uds, uds),
            ],
            LiveEndpoints::unwatched(),
        ));

        // ws has failed and the host is still going.
        let stopped = tokio::time::timeout(Duration::from_millis(50), &mut all).await;
        assert!(stopped.is_err(), "one failure ended the host: {stopped:?}");

        fail_uds.send(()).expect("the uds loop is still running");
        let err = all.await.expect_err("the last transport failed");
        assert!(
            matches!(&err, HostError::Serve(e) if e.contains("the socket was removed")),
            "the last failure is the one reported, got {err}",
        );
    }

    /// iroh's old special case, now just the rule. A host whose only transport
    /// dies has nothing to degrade to, so it stops — where the previous code
    /// parked on `pending()` forever and left a process serving nobody with a
    /// zero exit still ahead of it.
    #[tokio::test]
    async fn a_lone_transport_that_fails_takes_the_host_with_it() {
        let err = serve_all(
            vec![TransportServe::new(
                "iroh",
                drops_iroh,
                dies("iroh serve: the endpoint closed"),
            )],
            LiveEndpoints::unwatched(),
        )
        .await
        .expect_err("nothing else was listening");
        assert!(
            matches!(&err, HostError::Serve(e) if e.contains("the endpoint closed")),
            "got {err}",
        );
    }

    /// A loop that ends *without* failing is gone in exactly the same way — it
    /// is not listening any more, whatever it thought of that. The survivor
    /// keeps the host up, and the host's result is the survivor's.
    #[tokio::test]
    async fn a_loop_that_ends_cleanly_is_dropped_like_any_other() {
        let (ask, uds) = answers();
        let ended_cleanly: Serve = Box::pin(async { Ok(()) });
        let mut all = Box::pin(serve_all(
            vec![
                TransportServe::new("ws", drops_ws, ended_cleanly),
                TransportServe::new("uds", drops_uds, uds),
            ],
            LiveEndpoints::unwatched(),
        ));

        let (reply_tx, reply_rx) = oneshot::channel();
        ask.send(reply_tx).expect("the uds loop is still running");
        let answered = tokio::select! {
            r = &mut all => panic!("the host stopped serving: {r:?}"),
            a = reply_rx => a.expect("the uds loop answered"),
        };
        assert_eq!(answered, 1);

        // Dropping the sender ends the last loop cleanly, and a clean end is a
        // clean stop rather than a restart signal.
        drop(ask);
        all.await.expect("the last loop ended without failing");
    }

    /// **The staleness, closed.** A transport dies, the host goes on serving —
    /// and what it advertises stops naming the transport that is gone, while
    /// the ones still answering are untouched.
    ///
    /// This is the half that used to be missing. The policy above already said
    /// the survivors keep answering; what nobody was told was that the dead
    /// transport's URL stayed in the registry, so a consumer could be handed an
    /// address whose only possible outcome was a failed dial.
    #[tokio::test]
    async fn a_dead_transport_is_withdrawn_from_the_published_endpoints() {
        let (ask, uds) = answers();
        let (live, mut published) = LiveEndpoints::watched(vec![everywhere()]);
        let mut all = Box::pin(serve_all(
            vec![
                TransportServe::new("ws", drops_ws, dies("ws serve: listener gone")),
                TransportServe::new("uds", drops_uds, uds),
            ],
            live,
        ));

        // A round trip proves the host is still serving *and* drives the
        // combinator far enough to have absorbed the ws failure.
        let (reply_tx, reply_rx) = oneshot::channel();
        ask.send(reply_tx).expect("the uds loop is still running");
        tokio::select! {
            r = &mut all => panic!("the host stopped serving: {r:?}"),
            a = reply_rx => a.expect("the uds loop answered"),
        };

        let changed = tokio::select! {
            r = &mut all => panic!("the host stopped serving: {r:?}"),
            c = published.changed() => c,
        };
        changed.expect("the withdrawal was published");
        let now = published.borrow_and_update();
        let svc = now.first().expect("one service");
        assert!(
            svc.ws.is_empty(),
            "the dead transport's coordinates are still advertised: {:?}",
            svc.ws
        );
        assert_eq!(
            svc.uds,
            everywhere().uds,
            "a transport that is still answering must keep its coordinates"
        );
        assert_eq!(svc.iroh, everywhere().iroh, "and so must iroh");
        assert_eq!(
            svc.host,
            everywhere().host,
            "the host token names a filesystem namespace, not a listener; losing a \
             transport does not move this process to another machine"
        );
    }

    /// A transport whose death removes nothing publishes nothing. A host with
    /// no iroh coordinate to lose must not wake its coordinator client to send
    /// a `coord.update` restating what the registry already holds.
    #[tokio::test]
    async fn withdrawing_a_transport_that_contributed_nothing_publishes_no_change() {
        let mut only_uds = everywhere();
        only_uds.ws.clear();
        only_uds.iroh.clear();

        let (ask, uds) = answers();
        let (live, published) = LiveEndpoints::watched(vec![only_uds]);
        let mut all = Box::pin(serve_all(
            vec![
                TransportServe::new("iroh", drops_iroh, dies("iroh serve: the endpoint closed")),
                TransportServe::new("uds", drops_uds, uds),
            ],
            live,
        ));

        let (reply_tx, reply_rx) = oneshot::channel();
        ask.send(reply_tx).expect("the uds loop is still running");
        tokio::select! {
            r = &mut all => panic!("the host stopped serving: {r:?}"),
            a = reply_rx => a.expect("the uds loop answered"),
        };

        assert!(
            !published.has_changed().expect("the sender is alive"),
            "a transport that advertised nothing withdrew something"
        );
    }

    /// Nothing to serve resolves rather than parks. Unreachable through the
    /// host — a bind that produced no listener is already an error — and
    /// pinned anyway, because the alternative failure mode is a hang, which is
    /// the one thing a test would not notice.
    #[tokio::test]
    async fn serving_nothing_resolves_immediately() {
        let served = tokio::time::timeout(
            Duration::from_millis(50),
            serve_all(Vec::new(), LiveEndpoints::unwatched()),
        )
        .await;
        served
            .expect("an empty set does not park")
            .expect("and it is not a failure");
    }

    /// Two live loops and no failures: the host serves on, which is the
    /// ordinary case and the one a policy about failures could most easily
    /// break.
    #[tokio::test]
    async fn nothing_failing_serves_forever() {
        let all = serve_all(
            vec![
                TransportServe::new("ws", drops_ws, lives()),
                TransportServe::new("uds", drops_uds, lives()),
            ],
            LiveEndpoints::unwatched(),
        );
        let stopped = tokio::time::timeout(Duration::from_millis(50), all).await;
        assert!(stopped.is_err(), "the host resolved: {stopped:?}");
    }
}
