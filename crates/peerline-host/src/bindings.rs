//! Resolved dependencies, and the connections a service opens to them.
//!
//! A consumer never names an instance: it declares a *contract*
//! ([`Service::consumes`]), the coordinator picks a ready producer, and the
//! result arrives here as a [`Binding`]. [`Bindings`] is what
//! [`Service::bind`] is handed — the whole set, once every required contract
//! has resolved and before any transport accepts, so a handler can never
//! observe an unbound dependency.
//!
//! [`Service::consumes`]: crate::Service::consumes
//! [`Service::bind`]: crate::Service::bind
//!
//! # Why `connect` hands back a `Connection` and not a `Peer`
//!
//! Every transport's `connect` yields `(Peer, driver)`, and the driver has to
//! be owned and polled by somebody. Handing out a bare `Peer` would leave it
//! unowned — and worse, would throw away the one signal the failure model is
//! built on: **the driver resolving is the dependency dying**. [`Connection`]
//! owns the driver task and exposes that instant as [`Connection::closed`], so
//! a service can re-dial and the host can start its re-resolve budget.
//!
//! # The two ways a dependency is unreachable, and the way back
//!
//! A connection that was established and then dropped is one of them. A dial
//! that **never succeeded** is the other, and it is the one that used to be
//! invisible: nothing reported it, so the host's re-resolve budget never
//! started, the process never exited, and a binding that was wrong from the
//! first instant stayed in place for the life of the host while every
//! observability surface reported it healthy. Both now report through the same
//! channel (see [`Bindings::connect`]).
//!
//! And the way back is [`Bindings::refreshed`]: re-resolving is the *host's*
//! job and finishes asynchronously, so a service that reacts to `closed()` by
//! immediately re-dialling races it and re-dials the dead coordinates.
//! `refreshed` is how a service waits for the answer instead of racing it.
//!
//! # What `optional` means once a contract has resolved
//!
//! [`Consumed::optional`] used to govern *startup* alone: the coordinator's
//! `await_ready` does not wait for one, so an optional contract with no
//! producer yields no binding and the host becomes ready anyway. That left the
//! name lying about the interesting half of its life. An optional contract that
//! **did** resolve and then became unreachable reported exactly like a required
//! one — the same re-resolve budget, and the same process exit when it ran out
//! — so a consumer that declared a dependency optional precisely so it could
//! survive losing it was killed by losing it.
//!
//! So [`Bindings`] carries the [`Consumed`] declarations, not only the
//! [`Binding`]s they resolved to, and [`Bindings::is_optional`] is the answer.
//! A lost optional contract is re-resolved on exactly the same schedule, and
//! `connect` still returns `Err` while it is down — the caller always learns.
//! What changes is only the end of the budget: a required contract that never
//! comes back exits the host, an optional one degrades it.
//!
//! Optionality is **per host**, not per service: a host awaits the union of its
//! services' `consumes`, so one service declaring a contract required makes it
//! required for the process, whatever a second service in the same binary says.
//! That is the same reading §6 already gives across hosts, applied within one.
//!
//! [`Consumed::optional`]: peerline_contract::Consumed::optional
//!
//! # Dialling something that is not a contract
//!
//! [`Connection::dial`] is the way in for coordinates that arrived out of band
//! — from a parent process, a config file, another service's reply — rather
//! than from a resolved contract. It is the same transport preference and the
//! same [`HostId`] check, and it exists so that a caller in that position does
//! not have to reimplement either. Such a connection reports **no** loss: there
//! is no contract behind it, so there is nothing for the host to re-resolve and
//! no budget to start. [`Connection::closed`] still fires, which is the whole
//! signal such a caller has.
//!
//! # Transport preference, and the one rule that is not a preference
//!
//! uds, then ws, then iroh — cheapest first, and within a transport the first
//! coordinate is preferred with the rest as fallbacks. But uds is considered
//! **only** when the producer reported the same [`HostId`] as ours: a uds path
//! is a bare name in somebody's filesystem, and dialling one across a namespace
//! boundary does not fail cleanly, it connects to whatever unrelated socket
//! sits at that path. [`Endpoints::uds_usable_from`] is that comparison,
//! spelled once.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

use peerline::runtime::Peer;
use peerline_contract::{Binding, Consumed, ContractId, Endpoints, HostId};
use tokio::sync::{mpsc, watch};

/// A dependency's connection dropped: the contract it was for.
pub(crate) type Lost = mpsc::UnboundedSender<ContractId>;
/// The host's end of [`Lost`].
pub(crate) type LostRx = mpsc::UnboundedReceiver<ContractId>;

/// Every contract this host consumes that has resolved, and the means to dial
/// them.
///
/// Cheap to clone — a service that needs to dial later stores a clone from
/// [`Service::bind`](crate::Service::bind) rather than the `Binding` alone, so
/// that a re-dial after a producer restart picks up the *re-resolved*
/// coordinates rather than the stale ones it was first given.
#[derive(Clone)]
pub struct Bindings {
    inner: Arc<Inner>,
}

struct Inner {
    /// Our own locality tag, i.e. the answer to "may I use that uds path".
    host: HostId,
    /// Resolved bindings, replaced in place by the host's re-resolve loop,
    /// each with the number of times it has been replaced.
    map: Mutex<BTreeMap<ContractId, Resolved>>,
    /// Every contract this host consumes that **no** service of it declared
    /// required. Losing one of these is survivable: see the module docs.
    ///
    /// Held separately from `map` because it answers a question about the
    /// *declaration*, which exists whether or not the contract ever resolved.
    optional: BTreeSet<ContractId>,
    /// Tells the host a dependency's connection ended — or could not be
    /// established in the first place.
    lost: Lost,
    /// Bumped on every [`Bindings::replace`], so a service can await the
    /// re-resolve rather than race it. See [`Bindings::refreshed`].
    tick: watch::Sender<u64>,
}

/// One resolved dependency and how many times it has been re-resolved.
struct Resolved {
    binding: Binding,
    revision: u64,
}

impl Bindings {
    /// Build the set the host will hand to its services, together with the
    /// receiver it watches for dropped dependency connections.
    ///
    /// `consumed` is the union of every mounted service's
    /// [`consumes`](crate::Service::consumes) — the *declarations*, not the
    /// bindings they produced. It is what makes [`is_optional`](Self::is_optional)
    /// answerable, and a contract is optional here only if no service in this
    /// host asked for it required.
    pub(crate) fn new(
        host: HostId,
        consumed: &[Consumed],
        resolved: Vec<Binding>,
    ) -> (Self, LostRx) {
        let (lost, rx) = mpsc::unbounded_channel();
        let map = resolved
            .into_iter()
            .map(|b| {
                (
                    b.contract.clone(),
                    Resolved {
                        binding: b,
                        revision: 0,
                    },
                )
            })
            .collect();
        // One service declaring a contract required makes it required for the
        // whole process: readiness, and the exit in §9, are both per-host.
        let mut optional = BTreeSet::new();
        let mut required = BTreeSet::new();
        for c in consumed {
            if c.optional {
                optional.insert(c.contract.clone());
            } else {
                required.insert(c.contract.clone());
            }
        }
        optional.retain(|c| !required.contains(c));
        (
            Self {
                inner: Arc::new(Inner {
                    host,
                    map: Mutex::new(map),
                    optional,
                    lost,
                    tick: watch::channel(0).0,
                }),
            },
            rx,
        )
    }

    /// Replace one contract's binding after a re-resolve, so the next
    /// [`connect`](Self::connect) dials the producer that is actually there.
    #[cfg(feature = "uds")]
    pub(crate) fn replace(&self, binding: Binding) {
        {
            let mut map = lock(&self.inner.map);
            let entry = map
                .entry(binding.contract.clone())
                .or_insert_with(|| Resolved {
                    binding: binding.clone(),
                    revision: 0,
                });
            entry.binding = binding;
            entry.revision += 1;
        }
        // Wakes everyone parked in `refreshed`. Sent after the map is written
        // and the guard released, so a waiter that runs immediately sees the
        // new binding rather than blocking on a lock this thread still holds.
        self.inner.tick.send_modify(|t| *t += 1);
    }

    /// How many times this contract's binding has been replaced by a
    /// re-resolve. `0` for one that has never moved, and for one that never
    /// resolved at all.
    ///
    /// Take this **before** waiting on
    /// [`Connection::closed`](Connection::closed) and hand it to
    /// [`refreshed`](Self::refreshed): that is what makes the wait immune to
    /// the re-resolve finishing first.
    pub fn revision(&self, name: &str, version: u32) -> u64 {
        lock(&self.inner.map)
            .get(&ContractId::new(name, version))
            .map(|r| r.revision)
            .unwrap_or(0)
    }

    /// Wait until this contract's binding has been re-resolved past `since`,
    /// and return its new revision.
    ///
    /// # Why this exists
    ///
    /// The documented reaction to a dependency dying is "call `connect`
    /// again". On its own that **races the host**: `Connection::closed` fires
    /// from the same task that told the host to start re-resolving, so the
    /// obvious loop re-dials the *old*, dead coordinates — succeeding only
    /// because it happens to keep retrying until the host wins the race, and
    /// spinning against a producer that has genuinely moved until then. There
    /// was no signal for "the re-resolve finished", so there was no way to
    /// write the loop correctly.
    ///
    /// ```no_run
    /// # async fn f(deps: peerline_host::Bindings) -> Result<(), String> {
    /// let mut seen = deps.revision("storage", 1);
    /// loop {
    ///     match deps.connect("storage", 1).await {
    ///         Ok(conn) => {
    ///             seen = deps.revision("storage", 1);
    ///             conn.closed().await;
    ///         }
    ///         Err(e) => tracing::warn!("{e}"),
    ///     }
    ///     // Park until the host has actually put a new binding in place.
    ///     // If it never can, the host exits when its budget is spent (§9)
    ///     // and this task dies with the process.
    ///     seen = deps.refreshed("storage", 1, seen).await;
    /// }
    /// # }
    /// ```
    ///
    /// Returns immediately if the binding has already moved past `since`.
    pub async fn refreshed(&self, name: &str, version: u32, since: u64) -> u64 {
        let contract = ContractId::new(name, version);
        let mut tick = self.inner.tick.subscribe();
        loop {
            let now = lock(&self.inner.map)
                .get(&contract)
                .map(|r| r.revision)
                .unwrap_or(0);
            if now > since {
                return now;
            }
            if tick.changed().await.is_err() {
                // The host is gone; nothing will ever refresh this again.
                return now;
            }
        }
    }

    /// This host's identity, as reported in its own [`Endpoints`].
    pub fn host(&self) -> &HostId {
        &self.inner.host
    }

    /// The binding for a contract this service consumes.
    ///
    /// Panics if the contract did not resolve, which for a **required**
    /// contract cannot happen: the host does not call
    /// [`Service::bind`](crate::Service::bind) until every one of them has. Use
    /// [`get`](Self::get) for a contract declared
    /// [`optional`](peerline_contract::Consumed::optional) — an optional
    /// contract that never resolved is exactly the `None` case, and the host
    /// becomes ready anyway.
    pub fn require(&self, name: &str, version: u32) -> Binding {
        self.get(name, version).unwrap_or_else(|| {
            panic!(
                "peerline-host: no binding for `{name}/{version}` — it is not \
                 declared in this service's `consumes`, or it is declared optional \
                 (use `Bindings::get`)"
            )
        })
    }

    /// The binding for a contract, or `None` if it did not resolve.
    pub fn get(&self, name: &str, version: u32) -> Option<Binding> {
        lock(&self.inner.map)
            .get(&ContractId::new(name, version))
            .map(|r| r.binding.clone())
    }

    /// Every contract that resolved.
    pub fn contracts(&self) -> Vec<ContractId> {
        lock(&self.inner.map).keys().cloned().collect()
    }

    /// Whether losing this contract is survivable — i.e. whether every service
    /// in this host declared it
    /// [`Consumed::optional`](peerline_contract::Consumed::optional).
    ///
    /// This is what the host's re-resolve budget consults when it runs out: a
    /// required contract that never comes back exits the process, an optional
    /// one leaves it running and degraded. A contract this host does not
    /// consume at all answers `false`, which is the conservative reading and
    /// costs nothing — there is no binding for it to lose.
    pub fn is_optional(&self, name: &str, version: u32) -> bool {
        self.optional(&ContractId::new(name, version))
    }

    /// [`is_optional`](Self::is_optional), for a `ContractId` already in hand.
    pub(crate) fn optional(&self, contract: &ContractId) -> bool {
        self.inner.optional.contains(contract)
    }

    /// Dial the producer bound to a contract.
    ///
    /// Tries this host's coordinates in preference order (see the module docs)
    /// and returns the first that answers. The returned [`Connection`] owns the
    /// session: drop it to close the wire, and await
    /// [`Connection::closed`](Connection::closed) to learn that the producer
    /// went away.
    ///
    /// When it does go away, the host re-resolves the contract in the
    /// background — so the right response to `closed()` is to await
    /// [`refreshed`](Self::refreshed) and call `connect` again, not to give
    /// up. Only when the host's re-resolve budget is spent does the process
    /// exit — and then only for a contract this host declared **required**.
    /// Losing an optional one is re-resolved identically and reported here
    /// identically; what it does not do is end the process. See
    /// [`is_optional`](Self::is_optional).
    ///
    /// **A failed dial starts that same machinery.** It is not only a *lost*
    /// connection that means the coordinates are wrong: coordinates that never
    /// worked in the first place are wrong in exactly the same way, and are
    /// the more likely case at startup — a producer that died between
    /// readying and this dial, or one restarted at a new ephemeral port
    /// between the resolve and here. Reporting only established-then-dropped
    /// connections left a binding that was never dialable frozen for the life
    /// of the process, with the re-resolve budget never started and the exit
    /// in §9 never taken.
    pub async fn connect(&self, name: &str, version: u32) -> Result<Connection, String> {
        let contract = ContractId::new(name, version);
        // A contract with no binding at all is not a dependency that broke:
        // it was never declared, or it is an optional one that never
        // resolved. There is nothing for the host to re-resolve, so this does
        // not report a loss.
        let binding = lock(&self.inner.map)
            .get(&contract)
            .map(|r| r.binding.clone())
            .ok_or_else(|| format!("peerline-host: no binding for `{contract}`"))?;
        let (peer, driver) = match dial(&self.inner.host, &binding.endpoints).await {
            Ok(pair) => pair,
            Err(e) => {
                let _ = self.inner.lost.send(contract.clone());
                return Err(format!(
                    "peerline-host: cannot reach `{contract}` (instance `{}`): {e}",
                    binding.instance
                ));
            }
        };
        Ok(Connection::spawn(
            peer,
            driver,
            Some((contract, self.inner.lost.clone())),
        ))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A live connection to a producer: the peer to call, plus the driver that
/// keeps it running.
///
/// Dropping it aborts the driver and closes the connection, and does **not**
/// count as the dependency dying — a service that closes a connection on
/// purpose is not reporting a failure.
pub struct Connection {
    peer: Peer,
    closed: watch::Receiver<bool>,
    /// Aborts the driver task, and with it the `lost` notification. Held for
    /// its `Drop`, never read.
    _driver: AbortOnDrop,
}

impl Connection {
    /// Dial a producer whose coordinates arrived **out of band** — from a
    /// parent process, a config file, another service's reply — rather than
    /// from a contract this host resolved.
    ///
    /// The transport preference and the [`HostId`] locality rule are the ones
    /// [`Bindings::connect`] uses, and that is the point: the alternative for a
    /// caller in this position is to reimplement both, and the second one
    /// cannot be got wrong safely — a uds path dialled across a namespace
    /// boundary does not fail, it reaches an unrelated socket. `host` is
    /// normally [`host_id()`](crate::host_id), this process's own tag.
    ///
    /// The connection reports **no loss**. There is no contract behind it, so
    /// there is nothing for the host to re-resolve and no budget for it to
    /// spend; [`closed`](Self::closed) still fires, and for such a caller it is
    /// the whole signal.
    ///
    /// ```no_run
    /// # async fn f(uds: &std::path::Path) -> Result<(), String> {
    /// use peerline_contract::Endpoints;
    /// use peerline_host::{Connection, host_id};
    ///
    /// let mut endpoints = Endpoints::new(host_id().clone());
    /// endpoints.uds.push(uds.display().to_string());
    /// let conn = Connection::dial(host_id(), &endpoints).await?;
    /// conn.closed().await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn dial(host: &HostId, endpoints: &Endpoints) -> Result<Connection, String> {
        let (peer, driver) = dial(host, endpoints)
            .await
            .map_err(|e| format!("peerline-host: cannot reach that producer: {e}"))?;
        Ok(Connection::spawn(peer, driver, None))
    }

    /// Own the driver, and — for a connection that stands for a contract —
    /// report its end to the host.
    ///
    /// `report` is `None` for a [`dial`](Self::dial)ed connection, which is why
    /// there is no second type for one: everything else about the two is
    /// identical, and a `ConnectionWithoutALoss` would duplicate `peer`,
    /// `closed`, `is_closed` and the `AbortOnDrop` to express one absent field.
    fn spawn(
        peer: Peer,
        driver: futures::future::BoxFuture<'static, ()>,
        report: Option<(ContractId, Lost)>,
    ) -> Self {
        let (tx, closed) = watch::channel(false);
        let task = tokio::spawn(async move {
            driver.await;
            // The host is told *before* the waiter is woken, deliberately: a
            // service that reacts to `closed()` by dropping the `Connection`
            // would otherwise abort this task before the notification went out,
            // and the host would never start re-resolving.
            //
            // Reaching here at all means the task was not aborted, i.e. the
            // `Connection` is still held: the producer went away rather than
            // being deliberately let go.
            if let Some((contract, lost)) = report {
                let _ = lost.send(contract);
            }
            let _ = tx.send(true);
        });
        Self {
            peer,
            closed,
            _driver: AbortOnDrop(task),
        }
    }

    /// The peer to issue calls on.
    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    /// Resolves when the connection ends — the producer exited, restarted, or
    /// the transport failed. This *is* the dependency-died signal; §9 of the
    /// cluster design is built on it.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut rx = self.closed.clone();
        async move {
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    return;
                }
            }
        }
    }

    /// Whether the connection has already ended.
    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Dial one producer's coordinates, in preference order.
#[cfg_attr(
    not(any(feature = "ws", feature = "uds", feature = "iroh")),
    allow(unused_variables, unused_mut)
)]
pub(crate) async fn dial(
    host: &HostId,
    endpoints: &Endpoints,
) -> Result<(Peer, futures::future::BoxFuture<'static, ()>), String> {
    let mut tried: Vec<String> = Vec::new();

    // uds first, and only where the locality tag says the path means anything
    // here. See the module docs: this comparison is the difference between a
    // failed dial and a silent mis-dial.
    #[cfg(feature = "uds")]
    if endpoints.uds_usable_from(host) {
        for path in &endpoints.uds {
            match peerline_transport_uds::connect(path).await {
                Ok(pair) => return Ok(pair),
                Err(e) => tried.push(format!("uds {path}: {e}")),
            }
        }
    }

    #[cfg(feature = "ws")]
    for url in &endpoints.ws {
        match peerline_transport_ws::connect(url).await {
            Ok(pair) => return Ok(pair),
            Err(e) => tried.push(format!("ws {url}: {e}")),
        }
    }

    #[cfg(feature = "iroh")]
    for d in &endpoints.iroh {
        // The ticket picks the mode: a relay-less one is a `LocalOnly`
        // ticket whose only path is direct, and dialing it on an endpoint
        // bound to n0's default relays would pay for a relay the producer
        // is not on. Everything else stays `Auto`.
        match peerline_transport_iroh::connect(
            &d.ticket,
            d.alpn.as_bytes(),
            peerline_transport_iroh::DialMode::for_ticket(&d.ticket),
        )
        .await
        {
            Ok(pair) => return Ok(pair),
            Err(e) => tried.push(format!("iroh {}: {e}", d.alpn)),
        }
    }

    if tried.is_empty() {
        // Either the producer reported nothing dialable, or every coordinate it
        // did report is on a transport this binary was built without.
        return Err(format!(
            "no usable coordinates (this host is `{host}`, the producer reported \
             {} ws / {} uds / {} iroh)",
            endpoints.ws.len(),
            endpoints.uds.len(),
            endpoints.iroh.len()
        ));
    }
    Err(tried.join("; "))
}

#[cfg(all(test, feature = "uds"))]
mod tests {
    use super::*;

    fn binding(instance: &str) -> Binding {
        let mut endpoints = Endpoints::new(HostId::new("h"));
        endpoints.uds.push(format!("/tmp/{instance}.sock"));
        Binding {
            contract: ContractId::new("storage", 1),
            instance: instance.into(),
            endpoints,
        }
    }

    /// Optionality is a property of the *declaration*, and it is per host.
    ///
    /// A host awaits the union of its services' `consumes`, so two services in
    /// one binary can disagree about one contract — and the strict reading has
    /// to win, or a second service would silently remove the exit its
    /// colleague's `Consumed::new` asked for. A contract this host does not
    /// consume at all is not optional either: there is no binding to lose.
    #[test]
    fn required_by_any_service_makes_a_contract_required_for_the_host() {
        let (bindings, _lost) = Bindings::new(
            HostId::new("h"),
            &[
                Consumed::optional("storage", 1),
                Consumed::new("storage", 1),
                Consumed::optional("metrics", 1),
            ],
            Vec::new(),
        );
        assert!(
            !bindings.is_optional("storage", 1),
            "one service asked for it required, so the process needs it"
        );
        assert!(bindings.is_optional("metrics", 1));
        assert!(
            !bindings.is_optional("never-heard-of-it", 1),
            "not consumed at all is not the same as survivable"
        );
    }

    /// The signal a service needs to stop racing the host's re-resolve.
    ///
    /// Without it the only correct-looking loop — react to `closed()` by
    /// re-dialling — dials the coordinates that just died, because the host's
    /// re-resolve has not finished yet. It survives by retrying, which is
    /// indistinguishable from working until the producer genuinely moves.
    #[tokio::test]
    async fn refreshed_resolves_exactly_once_the_binding_has_moved() {
        let (bindings, _lost) = Bindings::new(
            HostId::new("h"),
            &[Consumed::new("storage", 1)],
            vec![binding("storage-a")],
        );
        let before = bindings.revision("storage", 1);

        // Already-fresh is not a wait: a re-resolve that finished before the
        // service got round to asking must not park it until the next one.
        bindings.replace(binding("storage-b"));
        let after = tokio::time::timeout(
            Duration::from_millis(100),
            bindings.refreshed("storage", 1, before),
        )
        .await
        .expect("a binding that has already moved returns immediately");
        assert!(after > before);
        assert_eq!(bindings.get("storage", 1).unwrap().instance, "storage-b");

        // And a waiter parked *before* the replace is woken by it, seeing the
        // new binding rather than the one it was waiting away from.
        let waiter = {
            let bindings = bindings.clone();
            tokio::spawn(async move {
                let rev = bindings.refreshed("storage", 1, after).await;
                (rev, bindings.get("storage", 1).unwrap().instance)
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "nothing has moved yet");
        bindings.replace(binding("storage-c"));
        let (rev, instance) = tokio::time::timeout(Duration::from_millis(500), waiter)
            .await
            .expect("the replace must wake the waiter")
            .expect("waiter task");
        assert!(rev > after);
        assert_eq!(instance, "storage-c");
    }

    /// A failed dial is a lost dependency, whether or not a connection was
    /// ever established. A contract with no binding at all is not: there is
    /// nothing for the host to re-resolve.
    #[tokio::test]
    async fn a_failed_dial_reports_the_loss_and_an_unknown_contract_does_not() {
        let mut endpoints = Endpoints::new(HostId::new("h"));
        endpoints
            .uds
            .push("/nonexistent/peerline-test/nobody.sock".into());
        let (bindings, mut lost) = Bindings::new(
            HostId::new("h"),
            &[Consumed::new("storage", 1)],
            vec![Binding {
                contract: ContractId::new("storage", 1),
                instance: "storage-a".into(),
                endpoints,
            }],
        );

        assert!(
            bindings.connect("storage", 1).await.is_err(),
            "nothing is listening there"
        );
        assert_eq!(
            lost.try_recv().ok(),
            Some(ContractId::new("storage", 1)),
            "a dial that never succeeded leaves the binding just as wrong as \
             one that died, and must start the same re-resolve"
        );

        assert!(
            bindings.connect("nothing", 1).await.is_err(),
            "not a declared contract"
        );
        assert!(
            lost.try_recv().is_err(),
            "a contract that never resolved is not a dependency that broke"
        );
    }
}
