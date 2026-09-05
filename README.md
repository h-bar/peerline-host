# peerline-host

The service layer for [peerline]: a composable `Service` trait, a
multi-transport `Host` that mounts many services in one binary, and a live
registry of what's running.

The protocol itself — wire envelopes, the peer runtime and the WebSocket /
Unix-socket / iroh transports — lives in the sibling [`peerline`][peerline]
repository. The dependency arrow only points that way; nothing there depends
on anything here.

## Crates

| Crate | What it is |
| --- | --- |
| `peerline-host` | the `Service` trait and the `Host` builder |
| `peerline-manager-protocol` | wire schema for the registry — pure JSON shapes, no transport, no runtime |
| `peerline-manager` | the registry itself: a daemon, a web dashboard and the `peerline-mon` CLI |

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
host.run().await?;   // binds transports, serves until SIGINT
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

## `peerline-manager`

A live registry of the peerline services on a host, which is itself a peerline
service. Hosts register their dial coordinates and heartbeat to stay live —
anything not heard from within the TTL is expired lazily on the next query.
Clients call `manager.list` / `manager.lookup` to discover who's up and how to
reach them.

Heartbeat rather than a held-open stream because peerline drains in-flight
handlers on connection close, so a parked handler cannot detect a hard crash.

`Host::report_to` wires this up: a host auto-registers on start, with
`Host::no_report` to opt out.

```sh
just install     # build, install binaries, load the launchd agent / systemd unit
just status      # supervisor state, endpoints, log locations
just logs        # tail
just uninstall
```

The daemon is env-driven with canonical defaults — UDS socket, WS RPC on
`127.0.0.1:6466`, dashboard on `http://127.0.0.1:6467/` — so nothing installs
a config file. Set `PEERLINE_MANAGER_*` in the unit or plist to override:

- `PEERLINE_MANAGER_SOCK=<path>` — Unix socket (default
  `/tmp/peerline-manager.sock`).
- `PEERLINE_MANAGER_WS=<addr:port>` — WebSocket RPC bind (default
  `127.0.0.1:6466`; `off` to disable). Needed by browser clients.
- `PEERLINE_MANAGER_UI=<addr:port>` — web dashboard bind (default
  `127.0.0.1:6467`; `off` to disable). Connects back to the WS port.
- `PEERLINE_MANAGER_ORIGINS=<url,url,…>` — browser origins admitted on the
  RPC port, overriding the default (the loopback spellings of the dashboard
  address). Required when the dashboard is exposed beyond loopback — set it
  to the URL browsers will actually load it from, e.g.
  `http://192.168.1.10:6467`.

## Development

Needs the sibling `peerline` repository checked out alongside this one:

```
libs/
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
