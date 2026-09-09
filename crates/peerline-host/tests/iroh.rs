//! What `Host` does with an iroh mount: it resolves the endpoint identity
//! while assembling transports — creating the key file owner-only on first
//! run and reusing it verbatim on the next, so the endpoint id (and thus
//! every ticket ever handed out) is stable across restarts — and it fails
//! the whole run rather than serving under a silently different identity
//! when that key, the relay URL or the ALPN set is wrong.
//!
//! Emitting the ticket used to need a relay, which made the end-to-end
//! assertion unrunnable here; `Host::iroh_reachability(LocalOnly)` publishes
//! a direct-address ticket instead, so
//! `ticket_carries_the_persisted_identity_and_dials_back` at the bottom now
//! runs offline like everything else in this file.

#![cfg(feature = "iroh")]

use std::path::{Path, PathBuf};

use peerline::runtime::Peer;
use peerline::wire::RpcError;
use peerline_host::{Host, IrohPolicy, Mount, Reachability, Service};
use peerline_transport_iroh::load_or_create_secret_key;
use serde::{Deserialize, Serialize};

const ALPN: &[u8] = b"peerline/host-test/1";

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

/// The one iroh mount every test here uses. `any_ticket_holder` is the
/// policy that matches what these tests prove — they are about the endpoint,
/// not about admission.
fn iroh_mount() -> Mount {
    Mount::new().iroh(ALPN, IrohPolicy::any_ticket_holder())
}

/// A scratch directory keyed by pid, so concurrent test binaries never
/// share a key file.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pl-host-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir scratch");
    dir
}

/// Run a host with one iroh mount and an already-resolved shutdown.
///
/// That is enough to observe the key, and deliberately so: `run_until`
/// resolves the endpoint identity while it assembles the transports, before
/// any of them is polled, so this exercises the real path without needing
/// the endpoint to reach a relay.
async fn run_iroh_host(key: &Path) -> Result<(), String> {
    Host::new()
        .mount(Echo, iroh_mount())
        .iroh_endpoint(Some(key.to_path_buf()))
        .run_until(std::future::ready(()))
        .await
}

/// The key is private key material for an identity clients pin, so where it
/// is written and who can read it are part of the contract — as is the fact
/// that a second run adopts it instead of minting a new one.
#[tokio::test]
async fn iroh_key_is_persisted_owner_only_and_reused_across_runs() {
    let dir = scratch("iroh-key");
    let key = dir.join("iroh.key");
    assert!(!key.exists(), "scratch dir must start clean");

    run_iroh_host(&key).await.expect("first run");

    let first = std::fs::read(&key).expect("the first run must create the key");
    assert_eq!(first.len(), 32, "an ed25519 secret key is 32 bytes");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key)
            .expect("stat key")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the key must not be readable by group or world (mode {mode:o})"
        );
    }
    let id_before = load_or_create_secret_key(Some(&key))
        .expect("read key")
        .public();

    run_iroh_host(&key).await.expect("second run");

    let second = std::fs::read(&key).expect("read key after second run");
    assert_eq!(
        first, second,
        "a second run must adopt the stored key, not overwrite it"
    );
    let id_after = load_or_create_secret_key(Some(&key))
        .expect("read key")
        .public();
    assert_eq!(
        id_before, id_after,
        "the same key path must yield the same endpoint identity"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A truncated or foreign key file must stop the run. Serving under a
/// freshly generated identity instead would silently invalidate every
/// ticket already handed out.
#[tokio::test]
async fn a_corrupt_iroh_key_fails_the_run() {
    let dir = scratch("iroh-corrupt");
    let key = dir.join("iroh.key");
    std::fs::write(&key, b"too short").expect("plant a bad key");

    let err = run_iroh_host(&key).await.unwrap_err();
    assert!(err.contains("iroh key"), "{err}");
    assert_eq!(
        std::fs::read(&key).expect("read key"),
        b"too short",
        "a rejected key file must be left as it was found"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Relay URLs are validated during assembly, not at first use, so a typo
/// fails the run instead of producing an endpoint nobody can reach.
#[tokio::test]
async fn a_bad_relay_url_fails_the_run() {
    let dir = scratch("iroh-relay");
    let key = dir.join("iroh.key");

    let err = Host::new()
        .mount(Echo, iroh_mount())
        .iroh_endpoint(Some(key.clone()))
        .iroh_relay("not a relay url")
        .run_until(std::future::ready(()))
        .await
        .unwrap_err();
    assert!(err.contains("iroh relay"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// One endpoint, one ticket, ALPN as the mount address — so two services
/// on one ALPN would shadow each other, exactly like two WS mounts on one
/// path.
#[tokio::test]
async fn two_services_cannot_share_one_iroh_alpn() {
    let dir = scratch("iroh-dupe");
    let key = dir.join("iroh.key");

    let err = Host::new()
        .mount(Echo, iroh_mount())
        .mount(Echo, iroh_mount())
        .iroh_endpoint(Some(key))
        .run_until(std::future::ready(()))
        .await
        .unwrap_err();
    assert!(err.contains("share iroh alpn"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// An iroh mount with no `iroh_endpoint` is inert, the same way a ws mount
/// with no `ws_bind` is — it does not half-start an endpoint.
#[tokio::test]
async fn an_iroh_mount_without_an_endpoint_is_not_an_active_mount() {
    let err = Host::new()
        .mount(Echo, iroh_mount())
        .run_until(std::future::ready(()))
        .await
        .unwrap_err();
    assert!(err.contains("no active mounts"), "{err}");
}

// --- what a failed iroh bind does to the rest of the host -------------------

/// A relay that cannot be reached, so the endpoint never acquires a relay
/// address and `bind_mounted` fails — deterministically, and without needing
/// the machine to be offline. The pinned relay mode means there is no fallback
/// to n0's defaults.
fn unreachable_relay() -> &'static str {
    "http://127.0.0.1:1"
}

/// **The rule that replaced parking forever.** A transport that fails to bind
/// is dropped: the host serves whatever did bind, and never advertises a
/// coordinate that does not work. Before this, a failed iroh endpoint was
/// logged and then parked on `pending()`, so a host could run "healthy" while
/// serving nothing.
#[tokio::test]
async fn a_failed_iroh_bind_leaves_the_other_transports_serving() {
    let dir = scratch("iroh-degrade");
    let sock = dir.join("echo.sock");

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (serve_sock, key) = (sock.clone(), dir.join("iroh.key"));
    let host = tokio::spawn(async move {
        Host::new()
            .mount(Echo, iroh_mount().uds(&serve_sock))
            .iroh_endpoint(Some(key))
            .iroh_relay(unreachable_relay())
            .run_until(async {
                let _ = rx.await;
            })
            .await
    });

    // uds bound in milliseconds; the iroh bind is still failing in the
    // background and must not hold this up or take it down.
    let (peer, driver) = 'dial: {
        for _ in 0..1000 {
            if let Ok(pair) = peerline_transport_uds::connect(&sock).await {
                break 'dial pair;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("uds never came up: {}", sock.display());
    };
    tokio::spawn(driver);
    let reply: String = peer
        .call("echo", &EchoArgs { s: "hi".into() })
        .await
        .expect("uds serves while iroh is failing to bind");
    assert_eq!(reply, "echo:hi");

    let _ = tx.send(());
    assert_eq!(host.await.expect("host task"), Ok(()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// ...and when iroh is the *only* transport, a failed bind is a failed run.
/// A host that cannot serve must say so rather than run healthy and answer
/// nobody — which is exactly what the old park-forever behaviour did.
#[tokio::test]
async fn an_iroh_only_host_that_cannot_bind_fails_the_run() {
    let dir = scratch("iroh-only-fails");
    let err = Host::new()
        .mount(Echo, iroh_mount())
        .iroh_endpoint(Some(dir.join("iroh.key")))
        .iroh_relay(unreachable_relay())
        // A shutdown that never fires: the run has to end on its own.
        .run_until(std::future::pending())
        .await
        .unwrap_err();
    assert!(err.contains("no active mounts"), "{err}");
    assert!(
        err.contains("iroh"),
        "the error must name what failed: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the networked half -----------------------------------------------------

/// A stand-in coordinator: it answers `coord.register` and hands the raw
/// registration JSON to the test. The ticket is now a value the host holds
/// the moment the endpoint binds — there is no `oneshot` from a bind callback
/// any more — but a registration is still the way to observe what the host
/// decided to *advertise*, which is the interesting half.
///
/// It speaks raw JSON rather than `peerline-contract`'s types so that only
/// this one handler, not the whole test, has to move when the registration
/// shape changes. `Echo` consumes nothing, so the host never calls
/// `coord.await_ready`, and `coord.ready` / `coord.heartbeat` are answered
/// only so the client does not treat this coordinator as broken.
struct CaptureCoordinator(tokio::sync::mpsc::UnboundedSender<serde_json::Value>);

impl Service for CaptureCoordinator {
    fn name(&self) -> &'static str {
        "coordinator"
    }
    fn register(&self, peer: &Peer) {
        use peerline_contract::coord::{OP_HEARTBEAT, OP_READY, OP_REGISTER};

        let tx = self.0.clone();
        peer.on_request(OP_REGISTER, move |reg: serde_json::Value| {
            let tx = tx.clone();
            async move {
                let id = reg["instance"].as_str().unwrap_or_default().to_string();
                let _ = tx.send(reg);
                Ok::<_, RpcError>(serde_json::json!({ "id": id }))
            }
        });
        peer.on_request(OP_READY, move |_: serde_json::Value| async move {
            Ok::<_, RpcError>(serde_json::json!({ "status": "ok" }))
        });
        // A request, not a notification (§6): the host reads the answer, and
        // a coordinator that does not reply is one that has stopped
        // answering.
        peer.on_request(OP_HEARTBEAT, move |_: serde_json::Value| async move {
            Ok::<_, RpcError>(serde_json::json!({ "status": "ok" }))
        });
    }
}

/// The whole iroh path end to end: the endpoint binds under the persisted
/// key, the ticket that reaches the coordinator carries that identity, and
/// dialing it with the mount's ALPN reaches the mounted service.
///
/// **Offline, and deliberately so.** Under the default
/// [`Reachability::Global`] this needed a reachable relay — the transport
/// refuses to publish a ticket without one, and a relay handshake cannot be
/// faked locally — so the assertion could not be made at all here.
/// `iroh_reachability(LocalOnly)` is the same host asking for the ticket it
/// can actually have on one machine: direct addresses, no relay wait. Every
/// assertion below is the one this test always made; what changed is that
/// the ticket now exists without a network.
///
/// It contacts no relay by construction, not by luck: none is configured, so
/// there is nothing to reach even on a connected machine, and the dial is
/// `DialMode::Direct`, which binds a relay-free endpoint and pins the dial
/// to the ticket's own direct addresses — the mode `LocalOnly` documents.
#[tokio::test]
async fn ticket_carries_the_persisted_identity_and_dials_back() {
    use std::time::Duration;

    let dir = scratch("iroh-ticket");
    let key = dir.join("iroh.key");
    let coord_sock = dir.join("coord.sock");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sock = coord_sock.clone();
    let coordinator = tokio::spawn(async move {
        Host::new()
            .mount(CaptureCoordinator(tx), Mount::new().uds(&sock))
            .run()
            .await
    });

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let (host_key, coord) = (key.clone(), coord_sock.clone());
    let host = tokio::spawn(async move {
        Host::new()
            .mount(Echo, iroh_mount())
            .iroh_endpoint(Some(host_key))
            // No relay is configured and none is waited for: the ticket is
            // this machine's direct addresses, which is all a same-host dial
            // needs.
            .iroh_reachability(Reachability::LocalOnly)
            .coordinator(coord)
            .run_until(async {
                let _ = stop_rx.await;
            })
            .await
    });

    let registration = tokio::time::timeout(Duration::from_secs(60), rx.recv())
        .await
        .expect("the host never registered")
        .expect("coordinator channel closed");

    let dial = &registration["services"][0]["endpoints"]["iroh"][0];
    assert_eq!(
        dial["alpn"].as_str().expect("alpn reported"),
        String::from_utf8_lossy(ALPN),
        "the ticket is reported against the mount's ALPN"
    );
    let ticket = dial["ticket"]
        .as_str()
        .expect("ticket reported")
        .to_string();
    assert!(
        ticket.starts_with(peerline_transport_iroh::TICKET_PREFIX),
        "{ticket}"
    );

    // The ticket names the identity stored at the key path — that is what
    // makes a ticket outlive a restart.
    let addr = peerline_transport_iroh::decode_ticket(&ticket).expect("decode ticket");
    let expected = load_or_create_secret_key(Some(&key))
        .expect("read key")
        .public();
    assert_eq!(
        addr.id, expected,
        "ticket must carry the persisted identity"
    );
    // ...and nothing else. No relay is configured and `LocalOnly` adds none,
    // so this is the structural proof that the test needs no network: the
    // published coordinate is direct addresses, and there is no relay in it
    // for either end to have contacted.
    assert_eq!(
        addr.relay_urls().count(),
        0,
        "a LocalOnly ticket names no relay: {addr:?}"
    );
    assert!(
        addr.ip_addrs().next().is_some(),
        "...and is made of direct addresses instead: {addr:?}"
    );

    // And it really is dialable, on the ALPN the mount was given.
    let (peer, driver) =
        peerline_transport_iroh::connect(&ticket, ALPN, peerline_transport_iroh::DialMode::Direct)
            .await
            .expect("dial the emitted ticket");
    tokio::spawn(driver);
    let reply: String = peer
        .call("echo", &EchoArgs { s: "hi".into() })
        .await
        .expect("the mounted service answers over iroh");
    assert_eq!(reply, "echo:hi");

    let _ = stop_tx.send(());
    assert_eq!(host.await.expect("host task"), Ok(()));
    coordinator.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
