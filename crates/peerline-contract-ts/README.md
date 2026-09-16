# peerline-contract-ts

TypeScript, generated from a peerline contract manifest — directly, without a
Rust mirror type or a `ts_rs` derive in between.

A contract manifest is a language-neutral JSON file describing one contract
completely enough to implement it in any language (the format lives in
`peerline-contract-spec`). This crate reads one and writes `.ts`: type aliases
for its structs, string-literal unions for its enums, discriminated unions for
its unions, intersections for its flattened fields, and the manifest's own
prose as doc comments on every one of them.

```console
$ peerline-contract-ts contracts/ bindings/
peerline-contract-ts: wrote 6 files to `bindings/`

$ peerline-contract-ts --check contracts/ bindings/
peerline-contract-ts: `bindings/` matches the contracts directory (6 files)
```

`--check` writes nothing, anywhere. It reports three things: a file whose
contents have drifted, a file that is generated and not committed, and a file
that is committed and no longer generated.

The full mapping table — every scalar, the three field states, both taggings,
flatten, imports — and the reasoning for each decision is in the crate docs
(`cargo doc --open -p peerline-contract-ts`).

## Three layouts, and the day two manifests share a name

The default layout is one flat file per type, because that is what the
existing `ts_rs` bindings are. Nothing in the format forbids two manifests in
one directory each declaring a `Session` — type names are unique *within* a
manifest — and one flat directory cannot hold both, so that layout refuses
such a directory with an error naming the type, both manifests, and the two
ways out:

```console
$ peerline-contract-ts contracts/ bindings/
peerline-contract-ts: `Session` is declared by `coagent.events/1` and by
`coagent.rpc/1`, and one file per type would write both to `Session.ts` —
whichever sorts last would silently win. Use the folder layout
(`--layout folder`), which gives every manifest its own directory, or narrow
the run to one of them (`--only`)
```

`--layout folder` is a folder per manifest, one file per type inside it. The
folder is the manifest's own file stem — `coagent.rpc.v1`, a name the format
already owns, since the directory loader requires `<contract>.v<version>.json`
— and collisions are impossible by construction: within a folder the
validator holds names unique, and across folders the filesystem does. The
files are byte-identical to the flat layout's except that a cross-manifest
import crosses folders (`../coagent.vocab.v1/SessionId`). With `--barrel`,
each folder gets an `index.ts` and the root gets namespace re-exports —
`export type * as CoagentRpc from "./coagent.rpc.v1/index";` (TypeScript
5.0), with a `V<n>` suffix on the namespace only when the directory holds the
same contract at more than one version. The alternatives — prefixed flat
filenames, renaming a colliding type on the fly — are rejected in the crate
docs, next to the layout's reasoning.

`--only <contract>[/<version>]` (repeatable) generates a subset under any
layout: the named contracts **plus their transitive imports, always**,
because the emitted files import those shapes by relative path and a subset
that dangled would only be caught by a downstream type-checker. A bare name
is every version the directory holds; an unknown name or version is an error
listing what the directory does hold.

## The `typescript { … }` clause is not going anywhere

There is an older route from a manifest to TypeScript: `contract!` expands the
manifest into Rust, a `typescript { … }` clause puts `#[derive(ts_rs::TS)]` on
some of those types, and a separate binary runs `TS::export_all`. That route
still works and this crate does not touch it. The output layout and naming here
deliberately match what it produces, so that a downstream swapping generators
diffs file *contents* rather than a reorganised tree.

`tests/compat.rs` holds a snapshot of a real downstream's manifest and of the
`ts_rs` output it produced, and asserts that the two generators declare the
same types — with every layout difference enumerated individually so the list
cannot quietly grow.

## A note for whoever next edits `peerline-contract-macros`

`src/scalar.rs` here is a second copy of that crate's `ScalarTarget` TypeScript
column. It has to be a copy: a proc-macro crate exports nothing but macros, so
that table is unreachable from ordinary code by construction. The two are kept
in step by hand, protected by an exhaustive match over `Scalar` in each (a new
keyword is a compile error in both) and by a cross-check test here.

**A one-line pointer comment on `ScalarTarget::ts` naming
`peerline-contract-ts`'s `scalar.rs` would close the loop from the other
side.** It is not written because that crate is owned elsewhere; it is the one
change this work would like made there and did not make.
