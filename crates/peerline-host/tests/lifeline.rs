//! The host's half of §10.5: a lifeline at EOF stops a running host, and takes
//! the ordinary shutdown path when it does.
//!
//! # One test, one binary
//!
//! `PEERLINE_LIFELINE_FD` is process-wide, and the descriptor behind it is
//! taken exactly once per process — the same reason `tests/manifest.rs` is a
//! file of its own. A second test here would be watching a lifeline the first
//! had already consumed, and would pass or fail depending on the order the
//! harness happened to run them in.
//!
//! # Why no child process is involved
//!
//! A supervisor passes the read end across `fork`/`exec`; nothing about the
//! *host's* behaviour depends on that. What the host does is: read a
//! descriptor number out of the environment, check it is a pipe, and stop when
//! it reports EOF. All three are true of a pipe made in this process, so the
//! test makes one, names it, and closes the write end — which is precisely
//! what a dying supervisor does, minus the dying.
//!
//! The other half — that a real supervisor really does hand a real child a
//! real descriptor, and that a `SIGKILL`ed supervisor really does take its
//! children with it — is `peerline-cluster/tests/lifeline.rs`, where there is
//! a process to kill.

#![cfg(feature = "uds")]

use std::os::fd::{AsRawFd, IntoRawFd};
use std::time::Duration;

use peerline::runtime::Peer;
use peerline::wire::RpcError;
use peerline_host::{Host, Mount, Service};

struct Echo;

impl Service for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn register(&self, peer: &Peer) {
        peer.on_request("echo", |_: serde_json::Value| async move {
            Ok::<_, RpcError>("echo".to_string())
        });
    }
}

// Single-threaded on purpose: `set_var` below is only sound while nothing
// else can be in `getenv`, and a current-thread runtime has no worker threads
// to be in one.
#[tokio::test]
async fn a_host_stops_when_its_supervisor_lets_go_of_the_lifeline() {
    let dir = std::env::temp_dir().join(format!("pl-lifeline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir scratch");
    let sock = dir.join("echo.sock");

    let (reader, writer) = std::io::pipe().expect("a pipe");
    // The host takes ownership of this descriptor, exactly as it would of one
    // inherited across an exec — so the number is handed over and the
    // `PipeReader` is not allowed to close it a second time.
    let fd = reader.into_raw_fd();
    assert!(fd > 2, "a lifeline is never a standard stream");

    // SAFETY: this is the only `set_var` in the binary, and it runs on a
    // current-thread runtime before anything is spawned — so there is no other
    // thread that could be in `getenv`, which is the whole hazard edition 2024
    // wants acknowledged.
    unsafe {
        std::env::set_var(
            peerline_contract::lifeline::LIFELINE_FD_ENV,
            peerline_contract::lifeline::render(fd),
        );
    }

    let serving = sock.clone();
    let host = tokio::spawn(async move {
        Host::new()
            .mount(Echo, Mount::new().uds(&serving))
            // `pending()`, so that the *only* thing that can end this run is
            // the lifeline. A host whose own shutdown future never resolves is
            // the shape every supervised service has.
            .run_until(std::future::pending::<()>())
            .await
    });

    // Serving, on its own terms, before anything is closed: without this the
    // test could pass against a host that failed to start.
    let peer = dial(&sock).await;
    let answer: String = peer
        .call("echo", &serde_json::json!({}))
        .await
        .expect("the host is serving");
    assert_eq!(answer, "echo");

    // The supervisor dies. This is the whole event: no signal, no message, no
    // cooperation — the last write end of the pipe goes away.
    drop(writer);

    let outcome = tokio::time::timeout(Duration::from_secs(10), host)
        .await
        .expect("the host must stop when its lifeline closes")
        .expect("host task");
    assert!(
        outcome.is_ok(),
        "EOF on the lifeline is a clean stop, like SIGTERM — not a failure, which \
         a supervisor would read as `restart me` (§9)"
    );

    assert!(
        peerline_transport_uds::connect(&sock).await.is_err(),
        "the listener must be gone once run_until has returned"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The host that *has* no lifeline is every other test in this workspace, so
/// this one only has to pin the boundary: an environment naming a descriptor
/// that is not a pipe must be ignored, not acted on and not fatal.
#[test]
fn a_descriptor_that_is_not_a_pipe_is_not_a_lifeline() {
    let path = std::env::temp_dir().join(format!("pl-lifeline-file-{}", std::process::id()));
    let file = std::fs::File::create(&path).expect("a scratch file");
    let _ = std::fs::remove_file(&path);

    // Parsing is the contract's business and succeeds: the number is
    // well-formed and above stdio. What it names is the host's business, and
    // is checked before anything is owned or closed — this descriptor is still
    // ours at the end of the test, which is the assertion `drop` makes.
    let rendered = peerline_contract::lifeline::render(file.as_raw_fd());
    assert_eq!(
        peerline_contract::lifeline::parse(&rendered),
        Ok(file.as_raw_fd())
    );
    drop(file);
}

async fn dial(sock: &std::path::Path) -> Peer {
    for _ in 0..200 {
        if let Ok((peer, driver)) = peerline_transport_uds::connect(sock).await {
            tokio::spawn(driver);
            return peer;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the uds socket never came up: {}", sock.display());
}
