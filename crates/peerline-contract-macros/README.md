# peerline-contract-macros

The `contract!` proc macro: a
[peerline](https://crates.io/crates/peerline) contract manifest,
compiled.

```rust,ignore
peerline_contract::contract!("codex.server/1" in "contracts");
```

A call site names the **contracts directory** and which contract it wants
out of it, because the directory is the unit a contract is written in: the
manifests in one reference each other's shapes and share one error-code
space. Every file in it is read and checked together, so a file that does
not parse, a file whose name disagrees with what it declares, an import
naming a type that does not exist, or two contracts picking the same error
code are all compile errors here.

The expansion is the op name constants, the argument / result / stream-item
types, the error codes, and the manifest's own prose as doc comments. The
types **are** the manifest, so they cannot drift from it — and the
expansion declares every file in the directory as a source input, so
editing any of them rebuilds the crate rather than leaving compiled types
describing a contract that no longer exists.

A manifest that imports shapes from another names it by contract and
version. Which Rust module supplies those shapes is a fact about the build
rather than about the contract, so it is written at the call site:

```rust,ignore
peerline_contract::contract!(
    "coagent.runtime/1" in "contracts",
    imports { "coagent.rpc/1" => ::coagent_rpc },
);
```

That clause carries no correctness, only the Rust path. The generated
derives are read off the manifest — a record of exact scalars carries `Eq`,
one holding an `f64` does not, one whose fields are all fixed-size is
`Copy` — and an imported shape is read the same way, out of the file that
declares it. None of those is a manifest key, because none of them is
something the contract says: they follow from what it already said.

Reach it through
[peerline-contract](https://crates.io/crates/peerline-contract), which
re-exports both this macro and the format it reads
([peerline-contract-spec](https://crates.io/crates/peerline-contract-spec)),
so a crate implementing a contract names one dependency.

License: MIT.
