# peerline-contract-ts

TypeScript, generated from a peerline contract manifest — directly, without a
Rust mirror type or a `ts_rs` derive in between.

A contract manifest is a language-neutral JSON file describing one contract
completely enough to implement it in any language (the format lives in
`peerline-contract-spec`). This crate reads one and writes `.ts`: type aliases
for its structs, string-literal unions for its enums, discriminated unions for
its unions, `Record<string, T>` for its maps, plain aliases for its newtypes,
intersections for its flattened fields, and the manifest's own prose as doc
comments on every one of them.

A **newtype** is a plain alias and not a branded type, and the asymmetry with
Rust is deliberate: every value of a generated type here arrives from
`JSON.parse`, a parsed string is a `string`, and a branded one would not be
assignable from it — so a consumer would cast at exactly the boundary the types
were meant to help at. The manifest's prose carries the distinction instead,
which is the answer `path` already gets for the same reason.

A field that is both `optional` and `default` is `key?: T` with the value in
the doc comment — the same rendering as a plain default, because the two differ
in a *direction* and TypeScript has one type for both. The line the type cannot
carry is written in the prose: that a producer really does omit the key, so an
absent one is normal traffic and not just a permission granted to callers.

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

## `--ops`, and why it is off by default

A manifest declares op names and error codes, and `--ops` emits them as
`export const`s — one file per contract, with the manifest's prose above each,
under the same constant names the Rust generator produces:

```ts
/**
 * The session's items, as they happen.
 *
 * Kind: stream.
 */
export const OP_WATCH = "coord.watch";
```

It is off by default because what makes this generator adoptable is that its
output can be diffed against what a downstream already committed, and what they
committed is shapes. With the flag off every byte of every file is what it has
always been; with it on, the constants arrive in files that did not exist
before, so nothing already there changes.

The file lands where each layout already puts per-manifest things:
`<contract>.v<n>.ops.ts` at the top level under `type` and `manifest`, and
`ops.ts` inside the folder under `folder`. Both are places `--check`'s staleness
rule already looks, so a stale ops file is reported with no new rule — and
`--only` governs these exactly as it governs types, because a subset selects
manifests. No barrel re-exports it: the barrels are `export type *`, erased at
compile time, and an ops file holds runtime values.

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

**That pointer is now written.** `ScalarTarget::ts` names this crate's
`scalar.rs` and the cross-check test by name, and `scalar.rs` names it back, so
whichever a reader reaches first tells them the other exists.

The same hand-sync applies to the **composites**, which are not in that column
at all: the macros crate's `ts_override` wraps a leaf answer as `Array<…>`,
`Record<string, …>` or `… | null`, and this crate spells them identically. A
downstream diffing the two generators has to see one shape described one way, so
a composite spelled differently would be a difference that is not one.
