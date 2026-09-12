//! What `Host::run_until` does when its shutdown future resolves: it
//! returns `Ok(())`, every listener it bound is gone, and connections that
//! were already established keep working — shutdown stops *accepting*, it
//! does not drain.
//!
//! The only test that exercised the shutdown arm of `run_until` used to live
//! in `peerline-manager` (`tests/report.rs`), which asserted deregistration
//! rather than shutdown itself. These prove the same arm using nothing but
//! `peerline-host` — which is what made deleting that crate safe.

#![cfg(all(feature = "uds", feature = "ws"))]

use std::time::Duration;

use peerline::runtime::Peer;
use peerline::wire::RpcError;
use peerline_host::{Host, Mount, Service, WsPolicy};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct EchoArgs {
    s: String,
}

/// A service whose `echo` tags its reply, so the test can tell a live
/// answer from a hung call.
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

/// A scratch directory keyed by pid, so concurrent test binaries never
/// share a socket path.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pl-host-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir scratch");
    dir
}

/// The uds listener binds inside the serve future, so it is not up the
/// instant `run_until` is spawned — retry a transient connect.
async fn dial_uds(sock: &std::path::Path) -> (Peer, futures::future::BoxFuture<'static, ()>) {
    for _ in 0..200 {
        if let Ok(pair) = peerline_transport_uds::connect(sock).await {
            return pair;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("uds socket never came up: {}", sock.display());
}

async fn dial_ws(url: &str) -> (Peer, futures::future::BoxFuture<'static, ()>) {
    for _ in 0..200 {
        if let Ok(pair) = peerline_transport_ws::connect(url).await {
            return pair;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("ws listener never came up: {url}");
}

/// Shutdown is observable in three ways, and all three are behaviour the
/// caller depends on: the future resolves `Ok`, the socket stops accepting,
/// and a connection that predates the signal is left alone.
#[tokio::test]
async fn shutdown_resolves_ok_stops_accepting_and_leaves_live_connections_alone() {
    let dir = scratch("shutdown");
    let sock = dir.join("echo.sock");

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let serve_sock = sock.clone();
    let host = tokio::spawn(async move {
        Host::new()
            .mount(Echo, Mount::new().uds(&serve_sock))
            .run_until(async {
                let _ = rx.await;
            })
            .await
    });

    let (peer, driver) = dial_uds(&sock).await;
    tokio::spawn(driver);
    let before: String = peer
        .call("echo", &EchoArgs { s: "before".into() })
        .await
        .expect("serving before shutdown");
    assert_eq!(before, "echo:before");

    tx.send(()).expect("host still running");

    // The shutdown arm returns `Ok`, not the "no active mounts" / transport
    // error the other arms produce.
    let outcome = host.await.expect("host task");
    assert!(outcome.is_ok(), "shutdown must be a clean exit");

    // `run_until` drops the serve futures before it returns, so the listener
    // is closed by the time we get here — no retry loop, a single refused
    // connect is the assertion.
    let refused = peerline_transport_uds::connect(&sock).await;
    assert!(
        refused.is_err(),
        "the uds listener must be gone once run_until returns"
    );

    // Current behaviour, worth pinning because it is easy to change by
    // accident: each accepted connection is served on its own detached task,
    // so shutdown neither closes nor drains it.
    let after: String = peer
        .call("echo", &EchoArgs { s: "after".into() })
        .await
        .expect("an already-established connection outlives shutdown");
    assert_eq!(after, "echo:after");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every transport the host bound goes down on one signal, not just the
/// first one in the list.
#[tokio::test]
async fn shutdown_tears_down_every_transport() {
    let dir = scratch("shutdown-multi");
    let sock = dir.join("echo.sock");

    // Ephemeral port, released before the host claims it (same probe the
    // ws test in `host.rs` uses).
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe bind");
    let addr = probe.local_addr().expect("local_addr");
    drop(probe);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let serve_sock = sock.clone();
    let host = tokio::spawn(async move {
        Host::new()
            .mount(
                Echo,
                Mount::new()
                    .uds(&serve_sock)
                    .ws("/echo", WsPolicy::allow_any()),
            )
            .ws_bind(addr)
            .run_until(async {
                let _ = rx.await;
            })
            .await
    });

    let url = format!("ws://{addr}/echo");
    let (ws_peer, ws_driver) = dial_ws(&url).await;
    tokio::spawn(ws_driver);
    let (uds_peer, uds_driver) = dial_uds(&sock).await;
    tokio::spawn(uds_driver);
    for peer in [&ws_peer, &uds_peer] {
        let r: String = peer
            .call("echo", &EchoArgs { s: "hi".into() })
            .await
            .expect("both transports serve the mount");
        assert_eq!(r, "echo:hi");
    }

    tx.send(()).expect("host still running");
    assert!(
        host.await.expect("host task").is_ok(),
        "a clean shutdown is Ok"
    );

    assert!(
        peerline_transport_uds::connect(&sock).await.is_err(),
        "uds listener must be gone"
    );
    assert!(
        peerline_transport_ws::connect(&url).await.is_err(),
        "ws listener must be gone"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A shutdown that is *already* resolved still wins: the host finishes
/// assembling its transports, then exits without serving. This is the arm a
/// restructure is most likely to break by awaiting a bind before entering
/// the select.
#[tokio::test]
async fn an_already_resolved_shutdown_stops_the_host_immediately() {
    let dir = scratch("shutdown-instant");
    let sock = dir.join("echo.sock");

    let outcome = Host::new()
        .mount(Echo, Mount::new().uds(&sock))
        .run_until(std::future::ready(()))
        .await;
    assert!(outcome.is_ok(), "a resolved shutdown is a clean exit");

    let _ = std::fs::remove_dir_all(&dir);
}
