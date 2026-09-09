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
host.run().await?;   // binds transports, serves until SIGINT / SIGTERM
```

The host owns the boilerplate each service used to hand-roll: binding
transports, admission, iroh key persistence and ticket emission, and
graceful shutdown.

A service that needs *another* service declares a contract rather than an
address, and opts into a coordinator:

```rust,ignore
host.coordinator("/tmp/peerline-coord.sock").run().await?;
// …or take the address a supervisor injected:
host.coordinator_from_env().run().await?;   // PEERLINE_COORD_SOCK
```

The host then binds, registers its real coordinates, waits for every
required contract to resolve, hands the resolved bindings to each service,
starts serving, and announces readiness — heartbeating throughout. Without
that call it makes no outbound connection at all. With
`PEERLINE_MANIFEST_OUT` set it writes what it produces and consumes to that
path and exits without binding anything.

`Host::run` stops on `SIGINT` **or `SIGTERM`** — a supervisor sends the
latter, and its default disposition would otherwise kill the process before
any of the shutdown path ran. A service that also honours
`PEERLINE_SERVICE_UDS` when choosing its mount address binds the address the
cluster config says it does.

See the [peerline-host repository](https://github.com/h-bar/peerline-host)
for the coordinator, and the
[peerline repository](https://github.com/h-bar/peerline) for the wire
format, the runtime, and the transports.

License: MIT.
