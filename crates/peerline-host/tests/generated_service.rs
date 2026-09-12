//! The generated runtime stubs, over a real peer.
//!
//! # Why this test is here and not in `peerline-contract`
//!
//! It needs three things at once: the macro, a peer runtime, and an async
//! test harness. `peerline-contract` has the first and deliberately not the
//! second — it does not depend on `peerline`, so that a dashboard or a
//! non-Rust-facing tool can hold a `Binding` without linking a transport.
//! Putting the test there would mean a `[dev-dependencies] peerline` arrow
//! from `peerline-contract` to `peerline`, and a dev-dependency arrow is
//! still an arrow: it is what `cargo test` resolves, what a vendoring build
//! has to fetch, and what somebody reading the manifest to check the layering
//! would find pointing the wrong way.
//!
//! `peerline-host` already depends on both, in the normal section, because
//! that is what a host *is*. So the test lands here and the graph is
//! unchanged.
//!
//! # What it asserts
//!
//! That the stubs run — not that they expand. What they expand to is pinned
//! in the macro crate's token tests, which can read a doc comment and an
//! attribute; what cannot be read there is whether the registration actually
//! answers, whether a stream op registered by the generator is answered as a
//! stream, and whether an op with no arguments or no result survives a round
//! trip.
//!
//! The per-op registrations are here for the same reason and one more: what a
//! *subset* is for cannot be read off tokens at all. That an op nobody
//! registered answers `MethodNotFound` — rather than answering, or failing in
//! some way that confirms it exists — is a property of the peer's dispatch,
//! and the only place to observe it is over a real connection.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use futures::StreamExt;
use peerline::runtime::{Peer, loopback};
use peerline::wire::RpcError;

mod demo {
    //! The fixture contract, expanded with the clause that asks for the
    //! trait, the registration and the client.
    peerline_contract::contract!("demo.host/1" in "tests/contracts", runtime);
}

use demo::{
    CountArgs, CountItems, DemoHostClient, DemoHostService, Fault, GreetArgs, Greeting, Tally, Tick,
};

/// One producer of `demo.host/1`, written the way the generated trait asks
/// for it: `async fn` per op, the manifest's shapes in and out, and a typed
/// sink for the stream.
#[derive(Default)]
struct Thing {
    greeted: AtomicU32,
}

impl DemoHostService for Thing {
    async fn greet(&self, args: GreetArgs) -> Result<Greeting, RpcError> {
        if args.name.is_empty() {
            return Err(RpcError {
                code: demo::ERR_DEMO_HOST,
                message: "a greeting needs a name".into(),
                data: serde_json::to_value(Fault::new("empty name")).ok(),
            });
        }
        self.greeted.fetch_add(1, Ordering::Relaxed);
        Ok(Greeting::new(format!("hello, {}", args.name)))
    }

    async fn forget(&self, _args: GreetArgs) -> Result<(), RpcError> {
        self.greeted.store(0, Ordering::Relaxed);
        Ok(())
    }

    async fn tally(&self) -> Result<Tally, RpcError> {
        Ok(Tally::new(self.greeted.load(Ordering::Relaxed)))
    }

    async fn count(&self, args: CountArgs, items: CountItems) -> Result<(), RpcError> {
        for n in 0..args.upto {
            // A send that fails means the consumer went away, which is not an
            // error to raise: there is nobody to raise it to.
            if items.send(&Tick::new(n)).is_err() {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// Two peers on an in-process pipe, with the generated registration on one
/// end and the generated client on the other. This is the whole seam under
/// test: nothing between the two spells an op name or a call kind.
///
/// The producer's `Peer` is handed back rather than dropped: it owns the
/// handler table, and a test that let it go would be testing what happens
/// when the far end disappears.
fn wired() -> (DemoHostClient, Arc<Thing>, Peer) {
    let (consumer, producer, driver) = loopback();
    tokio::spawn(driver);
    let thing = Arc::new(Thing::default());
    demo::register_demo_host_handlers(&producer, Arc::clone(&thing));
    (DemoHostClient::new(consumer), thing, producer)
}

/// The ordinary op: arguments in, a result out, through the trait the
/// manifest described and never through a `&str`.
#[tokio::test]
async fn a_unary_op_answers_through_the_generated_pair() {
    let (client, _thing, _producer) = wired();
    let greeting = client
        .greet(&GreetArgs::new("world"))
        .await
        .expect("the producer answered");
    assert_eq!(greeting.text, "hello, world");
}

/// An op whose manifest declares no `result` answers nothing at all, every
/// time. The generated trait spells that `()` rather than an `Option`, and
/// the round trip has to survive it — an empty result is a wire shape, and a
/// generator that got it wrong would fail here and nowhere else.
#[tokio::test]
async fn an_op_with_no_result_round_trips_as_nothing() {
    let (client, thing, _producer) = wired();
    client
        .greet(&GreetArgs::new("world"))
        .await
        .expect("greeted");
    assert_eq!(thing.greeted.load(Ordering::Relaxed), 1);

    client
        .forget(&GreetArgs::new("world"))
        .await
        .expect("forgotten");
    assert_eq!(thing.greeted.load(Ordering::Relaxed), 0);
}

/// The other absent shape: an op with no `args`. peerline refuses params that
/// are not a JSON object, so "no arguments" is an empty object and not a
/// missing one — the generated client sends `{}` and the generated handler
/// accepts it. A generator that sent `null` would be rejected by the peer
/// before the handler ran.
#[tokio::test]
async fn an_op_with_no_arguments_still_travels_as_an_object() {
    let (client, _thing, _producer) = wired();
    client
        .greet(&GreetArgs::new("world"))
        .await
        .expect("greeted");
    let tally = client.tally().await.expect("tallied");
    assert_eq!(tally.greeted, 1);
}

/// **The failure the whole clause exists for.** `demo.host.count` is declared
/// `stream`; a hand-written producer that reached for `on_request` would
/// answer this call with a kind mismatch at run time, on the first call, and
/// no argument or result schema would have caught it. The generated
/// registration reads the kind from the same manifest the op constant comes
/// from, so there is no line at which it could be wrong — and the assertion
/// that it was not is that items arrive at all.
#[tokio::test]
async fn a_stream_op_is_registered_as_a_stream() {
    let (client, _thing, _producer) = wired();
    let mut items = client.count(&CountArgs::new(4)).expect("the stream opened");

    let mut seen = Vec::new();
    while let Some(item) = items.next().await {
        seen.push(item.expect("an item, not a fault").data.n);
    }
    assert_eq!(seen, vec![0, 1, 2, 3]);
}

/// A producer's error reaches the consumer as the contract's own code, with
/// the typed payload the manifest says it carries. The generated client does
/// not flatten either: it hands back `peerline::runtime::Error`, and the
/// `Rpc` arm is where a caller matches on `ERR_…`.
#[tokio::test]
async fn a_contract_error_arrives_with_its_code_and_its_data() {
    let (client, _thing, _producer) = wired();
    let err = client
        .greet(&GreetArgs::new(""))
        .await
        .expect_err("an empty name is refused");
    match err {
        peerline::runtime::Error::Rpc(rpc) => {
            assert_eq!(rpc.code, demo::ERR_DEMO_HOST);
            let fault: Fault =
                serde_json::from_value(rpc.data.expect("the error carries data")).expect("a Fault");
            assert_eq!(fault.detail, "empty name");
        }
        other => panic!("expected an rpc error, got {other:?}"),
    }
}

/// The same two peers, with **part** of the contract on the producer: the two
/// ops a reader needs and neither of the two that change anything.
///
/// This is the shape a producer whose surface varies by transport builds — the
/// whole contract on a unix socket the kernel attests, a chosen few on a port
/// any local user can reach — written here with no host in the way, because
/// what is under test is the registration and not the admission.
fn wired_subset() -> (DemoHostClient, Arc<Thing>, Peer) {
    let (consumer, producer, driver) = loopback();
    tokio::spawn(driver);
    let thing = Arc::new(Thing::default());
    demo::register_greet(&producer, Arc::clone(&thing));
    demo::register_count(&producer, Arc::clone(&thing));
    (DemoHostClient::new(consumer), thing, producer)
}

/// **The property a subset is for.** What was registered answers; what was not
/// is `MethodNotFound`, which does not even confirm the op exists — no check
/// inside a handler, nothing to forget, and nothing on the wire that tells a
/// caller it found the right producer and was merely refused.
#[tokio::test]
async fn an_op_left_out_of_a_subset_is_not_there_at_all() {
    let (client, thing, _producer) = wired_subset();

    let greeting = client
        .greet(&GreetArgs::new("world"))
        .await
        .expect("`greet` was registered");
    assert_eq!(greeting.text, "hello, world");

    let err = client
        .forget(&GreetArgs::new("world"))
        .await
        .expect_err("`forget` was not registered");
    match err {
        peerline::runtime::Error::Rpc(rpc) => {
            assert_eq!(rpc.code, -32601, "{rpc:?}");
            assert_eq!(
                rpc.error_type(),
                peerline::wire::ErrorType::MethodNotFound,
                "{rpc:?}",
            );
        }
        other => panic!("expected MethodNotFound, got {other:?}"),
    }
    // And the op really did not run: `forget` would have reset this.
    assert_eq!(thing.greeted.load(Ordering::Relaxed), 1);
}

/// A per-op registration *is* the generated wiring, so a stream op registered
/// one at a time is still registered as a stream. A subset path that reached
/// for `on_request` would answer this with a kind mismatch — the failure the
/// whole clause exists to make unwritable, which a subset must not buy back.
#[tokio::test]
async fn a_subset_registers_a_stream_op_as_a_stream() {
    let (client, _thing, _producer) = wired_subset();
    let mut items = client.count(&CountArgs::new(3)).expect("the stream opened");

    let mut seen = Vec::new();
    while let Some(item) = items.next().await {
        seen.push(item.expect("an item, not a fault").data.n);
    }
    assert_eq!(seen, vec![0, 1, 2]);
}

/// The whole-contract registration is these same functions, called once each.
/// The assertion is that nothing is lost by the delegation: every op the
/// subset withheld answers when the whole contract is registered.
#[tokio::test]
async fn registering_every_op_is_what_the_whole_contract_registration_does() {
    let (client, _thing, _producer) = wired();
    client
        .greet(&GreetArgs::new("world"))
        .await
        .expect("greeted");
    assert_eq!(client.tally().await.expect("tallied").greeted, 1);
    client
        .forget(&GreetArgs::new("world"))
        .await
        .expect("`forget` is registered by the whole-contract path");
    assert_eq!(client.tally().await.expect("tallied").greeted, 0);
}

/// `produced()` is the declaration every producer used to write out by hand,
/// and the thing it must never disagree with is the op table beside it.
#[test]
fn produced_declares_the_contract_and_every_op() {
    let produced = demo::produced();
    assert_eq!(produced.contract.name, demo::CONTRACT);
    assert_eq!(produced.contract.version, demo::VERSION);
    assert_eq!(produced.ops, demo::OPS);
}

/// A generated producer is a `Service` without further ceremony: `produces`
/// is the generated declaration and `register` is the generated registration.
/// This is the shape the clause is meant to collapse a service down to, so it
/// is written out once here to prove it compiles.
struct ThingService {
    thing: Arc<Thing>,
}

impl peerline_host::Service for ThingService {
    fn name(&self) -> &'static str {
        "demo-host"
    }

    fn produces(&self) -> Vec<peerline_contract::Produced> {
        vec![demo::produced()]
    }

    fn register(&self, peer: &Peer) {
        demo::register_demo_host_handlers(peer, Arc::clone(&self.thing));
    }
}

#[tokio::test]
async fn a_service_is_the_generated_declaration_and_the_generated_registration() {
    use peerline_host::Service;

    let service = ThingService {
        thing: Arc::new(Thing::default()),
    };
    assert_eq!(service.produces(), vec![demo::produced()]);

    let (consumer, producer, driver) = loopback();
    tokio::spawn(driver);
    service.register(&producer);

    let client = DemoHostClient::new(consumer);
    let greeting = client
        .greet(&GreetArgs::new("service"))
        .await
        .expect("answered");
    assert_eq!(greeting.text, "hello, service");
}
