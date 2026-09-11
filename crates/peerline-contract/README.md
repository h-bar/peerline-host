# peerline-contract

Contract types for the [peerline](https://crates.io/crates/peerline)
cluster layer. A service declares the *contracts* it produces and
consumes — never the instance names of its dependencies — and a
coordinator answers with a `Binding`: which live producer it chose, and
how to dial it.

Pure serde JSON shapes — no transport, no runtime — so every party
shares one definition: services to declare, a coordinator to match, a
CLI or dashboard to display. Nothing here is Rust-specific.

Two rules are load-bearing and documented in the crate docs: producers
of one contract at one version must be **interchangeable**, and
`Endpoints` carries the reporting host's identity so that a `uds` path,
which has no machine identity of its own, is only ever dialled from the
same host.

Interchangeability is a precondition nothing can verify, but a producer
that admits no second instance can say so: `Produced::single(..)` sets
`Cardinality::Single`, and a coordinator then refuses the second live
producer at registration rather than silently ranking it second. It
defaults to `Cardinality::Any`, on the wire as well, so a declaration
written before the field existed keeps meaning what it meant.

It also holds the names of the environment variables a supervisor injects
into every child (`env`): the coordinator to register with, the socket to
serve on, and the manifest probe. Both ends of each of those live outside
this crate and neither may depend on the other, so a name spelled in either
one would have to be re-spelled in the other.

## Contract manifests

The types above are the *control* path — what a service declares and what
a coordinator answers. A contract's own vocabulary is not written in Rust
at all. It is a **contract manifest**: a language-neutral JSON file whose
format is `peerline-contract-spec`, from which the re-exported
`contract!` macro generates the Rust at compile time.

```rust,ignore
peerline_contract::contract!("contracts/codex.server.v1.json");
```

Op constants, the argument and result types, the error codes and the
documentation all come from that file, so they cannot drift from it — and
a service in another language reads the same file rather than reading the
Rust. Note that two different things here are called a manifest:
`manifest` is what a *binary* reports when probed, `spec` is what a
*contract* is.

See [peerline-host](https://crates.io/crates/peerline-host) for the
service layer, and the
[peerline repository](https://github.com/h-bar/peerline) for the wire
format underneath.

License: MIT.
