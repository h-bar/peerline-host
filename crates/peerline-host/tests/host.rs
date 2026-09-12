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
    use peerline_transport_ws::ConnectError;
    let mut attempt = 0u32;
    loop {
        match peerline_transport_ws::connect(url).await {
            Ok(pair) => break pair,
            // The listener may not be up yet — retry transient failures,
            // but a policy refusal is deterministic, so fail fast on it.
            Err(ConnectError::Other(_)) if attempt < 100 => {
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
        .ws_bind(addr);
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

/// Each of these used to be asserted with `err.contains("...")`, which tied
/// the test to wording that was never a promise. They are variants now, which
/// is what a downstream deciding whether to restart has to match on too.
#[tokio::test]
async fn run_rejects_empty_dupe_and_no_mount() {
    use peerline_host::{HostError, PlanError};

    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();

    // No services.
    let err = Host::new().ws_bind(addr).run().await.unwrap_err();
    assert!(matches!(err, HostError::NoServices), "{err}");

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
    assert!(
        matches!(
            &err,
            HostError::Plan(PlanError::AddressCollision {
                transport: "ws path",
                ..
            })
        ),
        "{err}"
    );

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
    assert!(matches!(err, HostError::NoMounts), "{err}");
}

/// The whole reason the bind/serve split exists: a host asked for port `0`
/// does not know its own address until the kernel has picked one, and
/// `ws_bind` took an address and handed nothing back.
///
/// The assertion is deliberately not "the port is non-zero" — that would pass
/// for any number at all. The port is *dialled*, and the service behind it
/// answers, which is the only evidence that the reported address is the one
/// the listener is really on.
#[tokio::test]
async fn bind_reports_the_port_the_kernel_actually_picked() {
    use peerline_host::Bound;

    let host = Host::new()
        .mount(
            Echo { name: "ephemeral" },
            Mount::new().ws("/e", WsPolicy::allow_any()),
        )
        // Port 0: the kernel chooses, and nothing above this line can know it.
        .ws_bind("127.0.0.1:0".parse().expect("a bind address"));

    let Bound::Listening(bound) = host.bind().await.expect("binds") else {
        panic!("no manifest probe and no supervisor: this host is listening")
    };

    let addr = bound.ws_addr().expect("a ws port was configured and bound");
    assert_ne!(addr.port(), 0, "the kernel has picked by now");

    // The same address, through the per-service endpoint report — which is
    // what would be registered with a coordinator, so the two must agree.
    let reported = bound
        .endpoints_for("ephemeral")
        .expect("the service is mounted");
    assert_eq!(
        reported.ws,
        vec![format!("ws://{addr}/e")],
        "the coordinate a consumer would be handed names the resolved port"
    );

    // Hold the bound host past the dial: the listener is open across the gap,
    // so a client that arrives before `serve` queues rather than being refused.
    let serving = tokio::spawn(async move { bound.serve_until(std::future::pending()).await });

    let (peer, driver) = dial(&format!("ws://{addr}/e")).await;
    tokio::spawn(driver);
    let reply: String = peer
        .call("echo", &EchoArgs { s: "hi".into() })
        .await
        .expect("the reported port is the one being served");
    assert_eq!(reply, "ephemeral:hi");

    serving.abort();
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
