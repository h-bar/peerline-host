//! The `contract!` macro: a contract manifest, compiled.
//!
//! Reach it through `peerline-contract`, which re-exports it, so that a crate
//! implementing a contract names one dependency rather than three.
//!
//! ```ignore
//! peerline_contract::contract!("contracts/codex.server.v1.json");
//! ```
//!
//! # What it expands to
//!
//! Everything the manifest describes, in the scope of the invocation:
//!
//! - `CONTRACT` and `VERSION`, carrying the contract's own prose as docs.
//! - One `OP_…` constant per op, documented with the manifest's prose plus a
//!   generated line naming the op's kind, argument, result and errors.
//! - `OPS`, the flat list a producer hands to `Produced::with_ops`, and
//!   `OP_KINDS`, the same list paired with each op's kind — see below.
//! - One `ERR_…` constant per declared error code.
//! - One Rust type per named shape, with a constructor, serde derives that
//!   reproduce the wire form exactly, and the manifest's prose on every type,
//!   field and variant.
//!
//! The types **are** the manifest: there is no second copy to keep in step,
//! so a manifest edit either changes the Rust or fails to compile.
//!
//! # Why `OP_KINDS` and not just `OPS`
//!
//! Unary and stream are different peerline calls, and a producer that
//! registers a stream op with `on_request` answers a `call_stream` with a kind
//! mismatch — at runtime, on the first call, in a way no argument or result
//! schema would have caught. `OP_KINDS` puts the manifest's answer where a
//! registration test can compare against it.
//!
//! # Why a manifest edit forces a rebuild
//!
//! A proc macro that reads a file does not, by itself, tell cargo that the
//! file is an input: nothing in the crate's source changed, so the crate is
//! not rebuilt, and the compiled types go on describing a manifest that no
//! longer exists. That is the worst failure available to a design whose whole
//! claim is that the two cannot drift.
//!
//! So the expansion ends with an `include_bytes!` of the manifest's resolved
//! path, bound to `const _`. The compiler records it as a source input in the
//! crate's dependency info, cargo reads that, and touching the manifest
//! rebuilds the crate. The constant itself is never read — it exists to be a
//! dependency edge.

#![forbid(unsafe_code)]

mod generate;

use std::path::PathBuf;

use proc_macro::TokenStream;
use syn::LitStr;

/// Expand a contract manifest into Rust.
///
/// Takes one string literal: the path to the manifest, relative to the
/// directory holding the invoking crate's `Cargo.toml` (an absolute path is
/// taken as-is). Crate-relative rather than file-relative because a manifest
/// is a property of the crate, not of whichever module happened to invoke the
/// macro.
#[proc_macro]
pub fn contract(input: TokenStream) -> TokenStream {
    let literal = syn::parse_macro_input!(input as LitStr);
    match expand(&literal) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(literal: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    let span = literal.span();
    let given = PathBuf::from(literal.value());
    let path = if given.is_absolute() {
        given
    } else {
        let root = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
            syn::Error::new(
                span,
                "CARGO_MANIFEST_DIR is not set, so a relative manifest path cannot be resolved; \
                 pass an absolute path",
            )
        })?;
        PathBuf::from(root).join(given)
    };

    let json = std::fs::read_to_string(&path)
        .map_err(|err| syn::Error::new(span, format!("cannot read `{}`: {err}", path.display())))?;
    let manifest = peerline_contract_spec::Manifest::parse(&json)
        .map_err(|err| syn::Error::new(span, format!("`{}`: {err}", path.display())))?;

    generate::contract(&manifest, &path, span)
}
