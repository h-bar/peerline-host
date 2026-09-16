# peerline-contract-spec

The **contract manifest** format for the
[peerline](https://crates.io/crates/peerline) cluster layer: a
language-neutral JSON description of one contract — its name and major
version, its ops, whether each is unary or a stream, the shapes their
arguments, results and stream items take, the error codes a producer may
raise, and the prose explaining why each of those is what it is.

The manifest is the contract. Rust is one implementation of it, generated
by `peerline_contract::contract!`; a service in any other language reads
the same file. Generating a spec *out of* Rust would keep Rust as the
authority, and that has a known failure — Rust can express things a target
cannot represent, and the divergence is only found when somebody tries.

This crate parses and validates — one manifest, and a whole directory of
them against each other, since an import, a file name and the error-code
space are only checkable with the directory in hand. It generates nothing:
the Rust generator is `peerline-contract-macros`, and both are re-exported by
[peerline-contract](https://crates.io/crates/peerline-contract), which is
the only one a consumer needs to name.

## What a manifest can say about the wire

The format describes wire forms that already exist, not only the ones it
would recommend, because a form it cannot describe is a contract that stays
undescribed. So a value's literal JSON text (`wire`) is separate from the
identifier every generator derives its own spelling from (`name`); a field
is required, absent (`optional`) or null (`nullable`); an absent key may
have a value (`default`), validated here against the field's declared type;
a union is tagged inside itself or by its key (`tagging`), and an arm may
carry one whole shape (`payload`) rather than restating its keys; and a
field's own keys may be written beside its siblings (`flatten`).

Where the format has a preference it says so and keeps both: **absent over
null** is the recommendation for a new contract, because a key that is not
there is the one form every target agrees on — and `nullable` exists
because contracts that emit `null` are real and describing them is the
point.

`optional` and `default` **combine**, and the pair is its own state: absence
is legal in both directions, and absence carries the value. That is what
serde spells `#[serde(default = …, skip_serializing_if = …)]` and it is the
commonest shape in the wires this format was written to describe. Until it
was expressible, such a field declared one half and put the other in prose
— a sentence no generator reads. The pair asks one thing of a field's shape
that no other state does: a writer deciding whether to omit the key has to
compare against the default, so a shape holding an `f64` or a passthrough
`json` is refused rather than given a predicate whose answer the contract
itself disclaims.

A value may be many values two ways: `{"array": …}` and `{"map": …}`. A
map's keys are **strings, always** — a JSON object has no other kind of
member name, so there is no key-type keyword and nothing one could say with
one. What the keys *mean* is prose on the field, where it already goes.

A named shape is a struct, an enum, a union, or a **newtype**: a name for a
shape that is already something else, wire-identical to what it wraps.
It adds nothing to the bytes and says that two values of one representation
are not one thing — a host id and a session id are both strings. Which
targets can act on that varies, and each generator answers from the same
declaration: Rust gets a distinct type because its newtypes are free,
TypeScript gets a plain alias because a branded type would not be
assignable from the parsed JSON every value arrives as.

## Error codes, and the range a contract claims

Two contracts in one directory may not pick the same code — a consumer
matches on the code and then decodes the payload behind it, so a collision
is a mis-decode rather than a confusing message. What that check cannot
refuse is the *next* collision: two contracts that do not collide today and
will, because neither said which numbers were its to spend.

An optional `codes` header — `{"start": 3100, "end": 3199}`, both ends
inclusive — is that convention written where a checker can read it.
Declaring one says every code in the file is inside it, and that no other
file in the directory may claim a number in it. Absent is today's behaviour
exactly, so a directory may hold banded and unbanded manifests at once and a
band can be added one file at a time.

A manifest may also declare types and no ops — a **vocabulary** — and
another manifest may `import` from it by contract name and version. Every
language-neutral IDL has this, for the same reason protobuf has `import`
and JSON Schema has `$ref`: the alternative is each contract restating the
shape, with nothing saying the restatements are the same one and nothing to
catch them drifting. What the manifest never names is a Rust path, a Python
package or a TypeScript module — that belongs wherever each generator is
invoked.

Two rules are worth knowing before writing a manifest. **Every
description is required** — the reasoning for a shape is contract
documentation, read by whoever implements it in whatever language, and a
manifest with an unexplained shape is rejected rather than accepted with a
warning. And the prose is **plain**: no rustdoc or markdown link syntax,
because its readers are not necessarily writing Rust.

License: MIT.
