# peerline-contract-macros

The `contract!` proc macro: a
[peerline](https://crates.io/crates/peerline) contract manifest,
compiled.

```rust,ignore
peerline_contract::contract!("contracts/codex.server.v1.json");
```

Expands to the op name constants, the argument / result / stream-item
types, the error codes, and the manifest's own prose as doc comments. The
types **are** the manifest, so they cannot drift from it — and the
expansion declares the manifest as a source input, so editing the file
rebuilds the crate rather than leaving compiled types describing a
contract that no longer exists.

Reach it through
[peerline-contract](https://crates.io/crates/peerline-contract), which
re-exports both this macro and the format it reads
([peerline-contract-spec](https://crates.io/crates/peerline-contract-spec)),
so a crate implementing a contract names one dependency.

License: MIT.
