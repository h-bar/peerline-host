# peerline-host

[![CI](https://github.com/h-bar/peerline-host/actions/workflows/ci.yml/badge.svg)](https://github.com/h-bar/peerline-host/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

The service layer for [peerline]: a composable `Service` trait, a
multi-transport `Host`, and a live registry of what's running.

The protocol itself — wire envelopes, the peer runtime and the WebSocket /
Unix-socket / iroh transports — lives in the sibling [`peerline`][peerline]
repository, and the cluster layer built on this one — the coordinator and the
supervisor — lives in [`peerline-cluster`][peerline-cluster]. The dependency
arrow only ever runs one way:

```
peerline-cluster → peerline-host → peerline
```

## Crates

| Crate | What it is |
| --- | --- |
| `peerline-host` | the `Service` trait and the `Host` builder |
| `peerline-contract` | what a service produces and consumes, the `coord.*` wire and the probe envelope — pure JSON shapes, no transport, no runtime |

`peerline-contract` lives here rather than with the coordinator that
implements it, because `peerline-host` depends on it: `Host` is a coordinator
client, and `Produced` / `Consumed` are on the `Service` trait. It is the
vocabulary a host speaks in order to take part in a cluster, so it belongs
with the host — and putting it in `peerline-cluster` would point an arrow back
and make the two repositories circular.

## `Host`

Every peerline service is "a function that registers handlers on a `Peer`".
`Service` turns that into a composable unit, and `Host` mounts several of them
behind one transport stack, so a fleet of services runs from one binary
instead of one binary each.

```rust
let mut host = Host::new().mount(
    Sh::new(state),
    Mount::new()
        .ws("/", WsPolicy::origins(["https://app.example"]))
        .uds(cfg.socket), // admits the same user (and root) by default
);
if let Some(addr) = cfg.ws_bind { host = host.ws_bind(addr); }
host.run().await?;   // binds transports, serves until SIGINT / SIGTERM
```

Each service is mounted at an address you configure, per transport, so a
connection reaches exactly one service and their ops never collide:

- **WebSocket** — a path on the shared port (`/` for a lone service, `/foo`
  when several share one).
- **Unix socket** — a socket file; the filesystem is the namespace.
- **iroh** — an ALPN on the shared endpoint: one ticket, dialer picks the ALPN.

Addresses are explicit rather than derived from the service name, so migrating
a single-service daemon keeps its exact wire addresses. The host owns the
boilerplate each service used to hand-roll: binding transports, requiring at
least one active mount, iroh key persistence and ticket emission, and graceful
shutdown.

### Admission

Every mount carries an admission policy — the transport's screening of who
gets a connection at all, applied before the service sees anything.
`Mount::ws` and `Mount::iroh` take theirs as a required argument (`WsPolicy`,
`IrohPolicy`): WebSocket is what a browser can reach and an iroh ticket is an
address rather than a credential, so neither has a default that is safe to
assume (`WsPolicy::allow_any()` / `IrohPolicy::any_ticket_holder()` exist and
say so out loud). `Mount::uds` defaults to `UdsPolicy::same_user()` — same
user and root, which the socket's `0600` permissions already enforce — with
`Mount::uds_policy` for anything else. The policies live in the `peerline`
transport crates (re-exported here); see the [peerline] docs for what each
one can and cannot establish.

All transports are on by default; a WS+UDS daemon can skip the iroh/QUIC stack
entirely with `default-features = false, features = ["ws", "uds"]`.

`Host` is not in the data path: it wraps each `Service::register` in a
closure and hands the collected mounts to the transport's `serve_mounted`.
Frames are the transport's business from there, so the protocol behaviour a
mounted service exhibits is exactly the transport's, tested in the `peerline`
repository. What is worth testing here is mount resolution — which path or
socket reaches which service — and that is what `tests/host.rs` covers.

## Joining a cluster

A host is complete on its own: mount some services, serve them, done. It can
also *take part in a cluster*, and the vocabulary for that is here even though
the cluster itself is not.

`Host::coordinator(path)` opts a host into a coordinator;
`Host::coordinator_from_env()` takes the path from `PEERLINE_COORD_SOCK`,
which is what a supervisor injects. A host that calls neither makes no
outbound connection and waits for nobody — which is how the coordinator's own
host avoids registering with itself. A host that does opt in registers what it
produces and consumes, heartbeats against a TTL, and receives its
dependencies' bindings as they resolve. The coordinator hands out coordinates
and is never in the data path, so its death never interrupts established
traffic.

When probed with `PEERLINE_MANIFEST_OUT` set, a `Host` writes what its
services produce and consume to that path and exits, which is how a supervisor
learns the dependency edges from the binaries rather than from a config file.

The other end of all of that — the coordinator daemon, `peerline-mon`, the
dashboard, the cluster config and the supervisor — lives in
[`peerline-cluster`][peerline-cluster], along with the host↔coordinator
integration battery, which needs a real one of each and so belongs on the side
of the arrow that can have both.

## Development

Needs the sibling `peerline` repository checked out alongside this one:

```
peerline/                  (any parent folder)
├── peerline/
└── peerline-host/
```

```sh
just test
just clippy
just fmt
```

## License

MIT.

[peerline]: https://github.com/h-bar/peerline
[peerline-cluster]: https://github.com/h-bar/peerline-cluster
