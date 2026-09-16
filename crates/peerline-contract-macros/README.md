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

## What the newer manifest forms become

- A **map** is `BTreeMap<String, V>`. Ordered and not hashed, because a wire
  type is serialised and an unordered map gives one value two encodings —
  which every byte comparison downstream depends on not happening. The `Ord`
  that buys is free: a map's keys are strings. Never `Copy`, and `Eq` exactly
  when the values are exact, like a list.
- A **newtype** is `#[serde(transparent)] pub struct HostId(pub String)`, with
  `new`, `into_inner`, `get` and `From` both ways. Wire-identical to what it
  wraps, and the comparison rung comes off the inner shape unchanged — wrapping
  an `i53` in a name does not cost the `Copy` it had.
- A field that is **`optional` with a `default`** is the bare type with
  `#[serde(default = "Ty::default_field", skip_serializing_if =
  "Ty::is_default_field")]`, where the predicate is generated beside the getter
  so the value a reader applies and the value a writer declines to write cannot
  get out of step. Not an `Option`: that would be a third state in the Rust the
  wire does not have. The one thing it asks of a shape is that the shape be
  comparable for exact equality, checked here with the whole directory open —
  which is the half the format's own validator cannot answer, because an
  imported shape's fields are in a file it did not parse.

Reach it through
[peerline-contract](https://crates.io/crates/peerline-contract), which
re-exports both this macro and the format it reads
([peerline-contract-spec](https://crates.io/crates/peerline-contract-spec)),
so a crate implementing a contract names one dependency.

License: MIT.
