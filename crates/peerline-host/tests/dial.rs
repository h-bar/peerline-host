//! `Connection::dial` — the public way in for coordinates that did **not**
//! come from a resolved contract.
//!
//! Everything else in this crate reaches a producer through
//! `Bindings::connect`, which needs a `Bindings` only the host can build. A
//! service handed an address out of band — by its parent, by a config file, by
//! another service's reply — had no way in at all, and the only thing it could
//! do was reimplement the dial: transport preference, the `HostId` locality
//! rule, and ownership of the driver whose resolution *is* the death signal.
//!
//! The second of those is the one that cannot be got wrong safely. A uds path
//! is a bare name in somebody's filesystem; dialled across a namespace
//! boundary it does not fail, it reaches whatever unrelated socket sits at that
//! path. `uds_usable_from` is the comparison, and the whole reason to have one
//! public dial rather than N private ones is that nobody has to remember it.

#![cfg(all(feature = "uds", feature = "ws"))]

use std::time::Duration;

use peerline::runtime::Peer;
use peerline::wire::RpcError;
use peerline_contract::{Endpoints, HostId};
use peerline_host::{Connection, Host, Mount, Service, WsPolicy, host_id};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct EchoArgs {
    s: String,
}

struct Echo;

impl Service for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn register(&self, peer: &Peer) {
        peer.on_request("echo", |a: EchoArgs| async move {
            Ok::<_, RpcError>(format!("echo:{}", a.s))
        });
    }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pl-dial-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir scratch");
    dir
}

/// Keep retrying until the host under test has bound, then hand back the
/// connection. A `Connection::dial` against a listener that is not up yet is a
/// legitimate failure, and this test is not about that race.
async fn dial_until_up(endpoints: &Endpoints) -> Connection {
    let mut last = String::new();
    for _ in 0..200 {
        match Connection::dial(host_id(), endpoints).await {
            Ok(conn) => return conn,
            Err(e) => {
                last = e.to_string();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    panic!("the producer never came up: {last}");
}

/// The uds path, taken straight from an `Endpoints` the way a caller who was
/// *handed* one would take it.
#[tokio::test]
async fn dial_reaches_a_producer_over_uds() {
    let dir = scratch("uds");
    let sock = dir.join("echo.sock");

    let serve = sock.clone();
    tokio::spawn(async move {
        Host::new()
            .mount(Echo, Mount::new().uds(&serve))
            .run()
            .await
    });

    let mut endpoints = Endpoints::new(host_id().clone());
    endpoints.uds.push(sock.display().to_string());

    let conn = dial_until_up(&endpoints).await;
    let answer: String = conn
        .peer()
        .call("echo", &EchoArgs { s: "uds".into() })
        .await
        .expect("echo");
    assert_eq!(answer, "echo:uds");
    assert!(!conn.is_closed());

    let _ = std::fs::remove_dir_all(&dir);
}

/// And over ws, which is the transport a caller on the *other* side of a
/// locality boundary has. Same call, same type, no branch in the caller.
#[tokio::test]
async fn dial_reaches_a_producer_over_ws() {
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe bind");
    let addr = probe.local_addr().expect("local_addr");
    drop(probe);

    tokio::spawn(async move {
        Host::new()
            .mount(Echo, Mount::new().ws("/echo", WsPolicy::allow_any()))
            .ws_bind(addr)
            .run()
            .await
    });

    // Tagged with a *foreign* host id on purpose: ws does not care, and this
    // is exactly the set a remote producer reports.
    let mut endpoints = Endpoints::new(HostId::new("somewhere-else"));
    endpoints.ws.push(format!("ws://{addr}/echo"));

    let conn = dial_until_up(&endpoints).await;
    let answer: String = conn
        .peer()
        .call("echo", &EchoArgs { s: "ws".into() })
        .await
        .expect("echo");
    assert_eq!(answer, "echo:ws");
}

/// The rule that is not a preference.
///
/// The socket here is **live and answering** — the previous test's producer in
/// miniature — so a dial that tried it would succeed. It must not be tried at
/// all, because the `HostId` says that path was named in a filesystem this
/// process does not share. That is the whole failure mode the locality tag
/// exists to prevent: not a failed dial, a *successful* one to the wrong
/// process.
#[tokio::test]
async fn dial_refuses_a_uds_path_across_a_hostid_mismatch() {
    let dir = scratch("mismatch");
    let sock = dir.join("echo.sock");

    let serve = sock.clone();
    tokio::spawn(async move {
        Host::new()
            .mount(Echo, Mount::new().uds(&serve))
            .run()
            .await
    });

    // Prove the socket really is reachable from here, so the refusal below
    // cannot be mistaken for the producer being down.
    let mut ours = Endpoints::new(host_id().clone());
    ours.uds.push(sock.display().to_string());
    let live = dial_until_up(&ours).await;
    let _: String = live
        .peer()
        .call("echo", &EchoArgs { s: "live".into() })
        .await
        .expect("the socket is answering");

    let mut theirs = Endpoints::new(HostId::new("another-namespace"));
    theirs.uds.push(sock.display().to_string());
    let refused = Connection::dial(host_id(), &theirs)
        .await
        .err()
        .expect("a uds path from another namespace must never be dialled");
    assert!(
        matches!(refused, peerline_host::DialError::NoCoordinates { .. }),
        "the path must be skipped outright rather than attempted and reported \
         as a failure — got: {refused}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `closed()` is the only signal such a caller gets: there is no contract
/// behind this connection, so nothing re-resolves it and nothing reports its
/// loss to a host. It has to fire.
///
/// The producer is a listener the test owns rather than a `Host`, for the
/// reason `peerline-cluster`'s dependency tests give: a host serves each
/// accepted connection on a detached task, so stopping one in-process does not
/// close the connections it already accepted. Dropping a stream this test holds
/// is a real disconnect.
#[tokio::test]
async fn closed_fires_when_the_producer_dies() {
    let dir = scratch("closed");
    let sock = dir.join("producer.sock");
    let listener = tokio::net::UnixListener::bind(&sock).expect("bind");

    let mut endpoints = Endpoints::new(host_id().clone());
    endpoints.uds.push(sock.display().to_string());

    let conn = Connection::dial(host_id(), &endpoints)
        .await
        .expect("the listener is up");
    let (held, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("the dial never arrived")
        .expect("accept");

    let closed = conn.closed();
    assert!(!conn.is_closed(), "nothing has died yet");
    drop(held);

    tokio::time::timeout(Duration::from_secs(5), closed)
        .await
        .expect("the producer went away and `closed()` must resolve");
    assert!(conn.is_closed());

    let _ = std::fs::remove_dir_all(&dir);
}
