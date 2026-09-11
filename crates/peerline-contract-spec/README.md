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

Two rules are worth knowing before writing a manifest. **Every
description is required** — the reasoning for a shape is contract
documentation, read by whoever implements it in whatever language, and a
manifest with an unexplained shape is rejected rather than accepted with a
warning. And the prose is **plain**: no rustdoc or markdown link syntax,
because its readers are not necessarily writing Rust.

License: MIT.
