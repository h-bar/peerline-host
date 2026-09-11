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

This crate parses and validates. It generates nothing: the Rust generator
is `peerline-contract-macros`, and both are re-exported by
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
