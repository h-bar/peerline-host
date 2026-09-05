# peerline-manager-protocol

Wire schema for the [peerline](https://crates.io/crates/peerline)
service manager: the register / list / lookup request and response
shapes, plus the `manager.*` method-name constants. Pure serde JSON
types — no transport, no runtime — so both sides of the manager link
share one definition: hosts depend on it to report in, clients to
query.

See [peerline-host](https://crates.io/crates/peerline-host) for the
service layer that speaks this protocol, and the
[peerline repository](https://github.com/h-bar/peerline) for the wire
format underneath.

License: MIT.
