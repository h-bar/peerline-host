# peerline-host

The service layer for [peerline](https://crates.io/crates/peerline) — a
composable `Service` trait plus a multi-transport `Host` that mounts many
services behind one transport stack (WebSocket path / Unix socket / iroh
ALPN), so a fleet of services runs from one binary instead of one binary
each. Each mount carries an admission policy (`WsPolicy`, `UdsPolicy`,
`IrohPolicy`), applied before a connection ever reaches the service.

```rust,ignore
use peerline_host::{Host, Mount, WsPolicy};

let mut host = Host::new().mount(
    Sh::new(state),
    Mount::new()
        .ws("/", WsPolicy::origins(["https://app.example"]))
        .uds(cfg.socket), // admits the same user (and root) by default
);
if let Some(addr) = cfg.ws_bind { host = host.ws_bind(addr); }
host.run().await?;   // binds transports, serves until SIGINT
```

The host owns the boilerplate each service used to hand-roll: binding
transports, admission, iroh key persistence and ticket emission, graceful
shutdown, and auto-registration with a `peerline-manager` registry.

See the [peerline-host repository](https://github.com/h-bar/peerline-host)
for the manager daemon and dashboard, and the
[peerline repository](https://github.com/h-bar/peerline) for the wire
format, the runtime, and the transports.

License: MIT.
