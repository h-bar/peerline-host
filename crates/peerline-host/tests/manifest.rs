//! The manifest probe: `PEERLINE_MANIFEST_OUT=/path fs-service` reports what
//! the binary declares and exits, without binding anything.
//!
//! This is its own test binary on purpose. The trigger is an environment
//! variable, which is process-wide, so a test that sets it must not share a
//! process with tests that expect a host to actually serve.

#![cfg(feature = "uds")]

use peerline::runtime::Peer;
use peerline::wire::RpcError;
use peerline_contract::manifest::{MANIFEST_OUT_ENV, Manifest};
use peerline_contract::{Consumed, ContractId, Produced};
use peerline_host::{Host, Mount, Service};

struct Storage;

impl Service for Storage {
    fn name(&self) -> &'static str {
        "storage"
    }
    fn produces(&self) -> Vec<Produced> {
        vec![Produced::with_ops("storage", 1, ["storage.get"])]
    }
    fn register(&self, peer: &Peer) {
        peer.on_request("storage.get", |_: serde_json::Value| async {
            Ok::<_, RpcError>("v")
        });
    }
}

struct Fs;

impl Service for Fs {
    fn name(&self) -> &'static str {
        "fs"
    }
    fn produces(&self) -> Vec<Produced> {
        vec![Produced::new("fs", 1)]
    }
    fn consumes(&self) -> Vec<Consumed> {
        vec![
            Consumed::new("storage", 1),
            Consumed::optional("metrics", 2),
        ]
    }
    fn register(&self, _peer: &Peer) {}
}

/// One test, because the variable is process-wide: the manifest a host reports
/// is every service it mounts, and asking for it binds nothing.
#[tokio::test]
async fn the_probe_reports_every_mounted_service_and_binds_nothing() {
    let dir = std::env::temp_dir().join(format!("pl-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let out = dir.join("manifest.json");
    let sock = dir.join("fs.sock");

    let host = || {
        Host::new()
            .mount(Storage, Mount::new().uds(dir.join("storage.sock")))
            .mount(Fs, Mount::new().uds(&sock))
            // A consumer, and deliberately *no* coordinator: the probe must
            // report before that configuration is even checked, or a service
            // could not be probed without a running cluster.
            .coordinator(dir.join("coord.sock"))
    };

    // The envelope is a list: one process, several mounted services.
    let manifest = host().manifest();
    assert_eq!(manifest.services.len(), 2);
    assert_eq!(manifest.services[0].service, "storage");
    assert_eq!(
        manifest.services[0].produces[0].contract,
        ContractId::new("storage", 1)
    );
    assert_eq!(manifest.services[0].produces[0].ops, vec!["storage.get"]);
    assert_eq!(manifest.services[1].service, "fs");
    assert_eq!(
        manifest.services[1].consumes[0].contract,
        ContractId::new("storage", 1)
    );
    assert!(!manifest.services[1].consumes[0].optional);
    assert!(manifest.services[1].consumes[1].optional);

    // SAFETY-of-the-suite: this is the only test in this binary, and it is the
    // whole reason the binary exists.
    unsafe { std::env::set_var(MANIFEST_OUT_ENV, &out) };

    // `run` would otherwise block until Ctrl-C. Under the variable it must
    // return promptly, cleanly, and without having bound a thing.
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), host().run())
        .await
        .expect("the probe must exit rather than serve");
    assert_eq!(outcome, Ok(()), "a probe is a clean exit");

    let written = Manifest::from_json(&std::fs::read(&out).expect("the probe wrote its manifest"))
        .expect("valid manifest json");
    assert_eq!(
        written, manifest,
        "the file is exactly what `manifest()` says"
    );

    assert!(
        !sock.exists(),
        "a probe must not bind anything: {}",
        sock.display()
    );

    unsafe { std::env::remove_var(MANIFEST_OUT_ENV) };
    let _ = std::fs::remove_dir_all(&dir);
}
