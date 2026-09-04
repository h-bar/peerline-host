//! End-to-end: two `Service`s registered on one `Host`, served over WS,
//! each reached at its own mount path and answering in isolation.

use std::time::Duration;

use peerline::runtime::Peer;
use peerline::wire::RpcError;
use peerline_host::{Host, Mount, Remote, Service, WsPolicy};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct EchoArgs {
    s: String,
}

/// A service whose `echo` tags its reply with the service name, so the
/// test can prove which mount answered.
struct Echo {
    name: &'static str,
}

impl Service for Echo {
    fn name(&self) -> &'static str {
        self.name
    }
    fn register(&self, peer: &Peer) {
        let tag = self.name;
        peer.on_request("echo", move |a: EchoArgs| async move {
            Ok::<_, RpcError>(format!("{tag}:{}", a.s))
        });
    }
}

async fn dial(url: &str) -> (Peer, futures::future::BoxFuture<'static, ()>) {
    let mut attempt = 0u32;
    loop {
        match peerline_transport_ws::connect(url).await {
            Ok(pair) => break pair,
            Err(_) if attempt < 100 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("connect {url}: {e}"),
        }
    }
}

#[tokio::test]
async fn host_mounts_two_services_by_path() {
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe bind");
    let addr = probe.local_addr().expect("local_addr");
    drop(probe);

    let host = Host::new()
        .mount(
            Echo { name: "alpha" },
            Mount::new().ws("/alpha", WsPolicy::allow_any()),
        )
        .mount(
            Echo { name: "beta" },
            Mount::new().ws("/beta", WsPolicy::allow_any()),
        )
        .ws_bind(addr)
        .no_report();
    tokio::spawn(async move {
        let _ = host.run().await;
    });

    let (alpha, d1) = dial(&format!("ws://{addr}/alpha")).await;
    tokio::spawn(d1);
    let (beta, d2) = dial(&format!("ws://{addr}/beta")).await;
    tokio::spawn(d2);

    let a: String = alpha
        .call("echo", &EchoArgs { s: "hi".into() })
        .await
        .expect("alpha");
    let b: String = beta
        .call("echo", &EchoArgs { s: "hi".into() })
        .await
        .expect("beta");
    assert_eq!(a, "alpha:hi");
    assert_eq!(b, "beta:hi");
}

#[tokio::test]
async fn run_rejects_empty_dupe_and_no_mount() {
    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();

    // No services.
    let err = Host::new().ws_bind(addr).run().await.unwrap_err();
    assert!(err.contains("no services"), "{err}");

    // Two services colliding on one WS path.
    let err = Host::new()
        .mount(
            Echo { name: "a" },
            Mount::new().ws("/same", WsPolicy::allow_any()),
        )
        .mount(
            Echo { name: "b" },
            Mount::new().ws("/same", WsPolicy::allow_any()),
        )
        .ws_bind(addr)
        .run()
        .await
        .unwrap_err();
    assert!(err.contains("share ws path"), "{err}");

    // A service exists but no transport is active (mount requests WS, host
    // never binds a WS port).
    let err = Host::new()
        .mount(
            Echo { name: "x" },
            Mount::new().ws("/x", WsPolicy::allow_any()),
        )
        .run()
        .await
        .unwrap_err();
    assert!(err.contains("no active mounts"), "{err}");
}

/// The uid `register_remote` saw, so the test can prove credentials
/// actually arrived rather than the branch being skipped.
static SEEN_UID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// A service that varies *what exists* by who connected, rather than
/// checking permissions inside a handler.
struct Guarded;

impl Service for Guarded {
    fn name(&self) -> &'static str {
        "guarded"
    }

    fn register(&self, peer: &Peer) {
        peer.on_request("public", |_: serde_json::Value| async {
            Ok::<_, RpcError>("open")
        });
    }

    fn register_remote(&self, peer: &Peer, remote: &Remote) {
        self.register(peer);
        // Over UDS the caller's uid is kernel-attested. Root is the
        // predicate here purely because the test is definitely not root,
        // so the privileged op must end up unregistered.
        let trusted = match remote {
            Remote::Uds(c) => {
                SEEN_UID.store(c.uid(), std::sync::atomic::Ordering::Release);
                c.uid() == 0
            }
            _ => false,
        };
        if trusted {
            peer.on_request("privileged", |_: serde_json::Value| async {
                Ok::<_, RpcError>("granted")
            });
        }
    }
}

/// An op the caller was not granted is not merely refused — it was never
/// registered, so it answers `MethodNotFound` and does not even confirm it
/// exists. That is the point of deciding at registration time.
#[tokio::test]
async fn register_remote_varies_the_op_set_by_identity() {
    let dir = std::env::temp_dir().join(format!("pl-host-guard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let sock = dir.join("guarded.sock");

    let serve_sock = sock.clone();
    let host = tokio::spawn(async move {
        Host::new()
            .mount(Guarded, Mount::new().uds(&serve_sock))
            .no_report()
            .run()
            .await
    });

    let mut connected = None;
    for _ in 0..200 {
        if let Ok(c) = peerline_transport_uds::connect(&sock).await {
            connected = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (peer, driver) = connected.expect("uds server should come up");
    tokio::spawn(driver);

    let open: String = peer
        .call("public", &serde_json::json!({}))
        .await
        .expect("the ungated op is registered for everyone");
    assert_eq!(open, "open");

    let denied = peer
        .call::<_, String>("privileged", &serde_json::json!({}))
        .await;
    assert!(
        denied.is_err(),
        "an op that was never registered must not answer"
    );

    // Credentials really arrived — the branch was taken, not skipped.
    let seen = SEEN_UID.load(std::sync::atomic::Ordering::Acquire);
    assert_ne!(seen, u32::MAX, "register_remote never saw a Uds remote");
    assert_ne!(seen, 0, "test should not be running as root");

    host.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
