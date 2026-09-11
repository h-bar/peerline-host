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

A manifest that imports shapes from another names it by contract and
version. Which Rust module supplies those shapes is a fact about the build
rather than about the contract, so it is written at the call site — as is
`supports`, which answers the one question an imported shape leaves open:
how far up the comparison ladder the Rust type goes, since its fields are in
a file this expansion never opens.

```rust,ignore
peerline_contract::contract!(
    "contracts/coagent.runtime.v1.json",
    imports { "coagent.rpc/1" => ::coagent_rpc },
    supports { SessionInfo: none },
);
```

Everything else about the generated derives is read off the manifest: a
record of exact scalars carries `Eq`, one holding an `f64` does not, and one
whose fields are all fixed-size is `Copy`. None of those is a key, because
none of them is something the contract says — they follow from what it
already said.

Reach it through
[peerline-contract](https://crates.io/crates/peerline-contract), which
re-exports both this macro and the format it reads
([peerline-contract-spec](https://crates.io/crates/peerline-contract-spec)),
so a crate implementing a contract names one dependency.

License: MIT.
