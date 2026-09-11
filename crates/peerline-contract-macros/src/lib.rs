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
//! A manifest whose `role` is `vocabulary` has no ops, so it expands to the
//! header and the shapes and nothing else: no `OPS` and no `OP_KINDS`, because
//! nobody produces a vocabulary and there is no registration for either list
//! to feed.
//!
//! # Why `OP_KINDS` and not just `OPS`
//!
//! Unary and stream are different peerline calls, and a producer that
//! registers a stream op with `on_request` answers a `call_stream` with a kind
//! mismatch — at runtime, on the first call, in a way no argument or result
//! schema would have caught. `OP_KINDS` puts the manifest's answer where a
//! registration test can compare against it.
//!
//! # Imports, and why the Rust path is written here
//!
//! A manifest that uses a shape from another one names it by contract and
//! version — `coagent.rpc/1` — and never by a Rust path, because the manifest
//! is read by implementors who are not writing Rust and a crate name would
//! mean nothing to them. Which Rust module supplies those types is a fact
//! about *this* build, so it is written at the call site:
//!
//! ```ignore
//! peerline_contract::contract!(
//!     "contracts/coagent.runtime.v1.json",
//!     imports {
//!         "coagent.rpc/1" => ::coagent_rpc,
//!     },
//! );
//! ```
//!
//! Every import the manifest declares must be mapped, and mapping one it does
//! not declare is an error rather than a no-op: both are how a renamed
//! vocabulary is caught at the call site instead of as a missing type later.
//!
//! A Python or TypeScript generator carries the same mapping in whatever form
//! suits it, reading the identical manifest. That is the general rule this
//! clause is an instance of: a key that only makes sense to one language does
//! not go in the shared file.
//!
//! # `supports`, and why derives are not in the manifest
//!
//! The comparison and copy derives on a generated type are read off the
//! *shape*: a record of exact scalars can carry `Eq`, one holding an `f64`
//! cannot, and neither fact is something the manifest states — they follow
//! from what it already says, which is why they are not keys. A manifest that
//! listed Rust derives would be a manifest with a Rust-shaped hole in it.
//!
//! An **imported** shape is the one case where the manifest does not say.
//! Its fields are in a file this expansion never opens, and the Rust type
//! supplying it may be hand-written and deliberately without `PartialEq`. So
//! the call site answers for it, in the language-neutral currency of how far
//! up the ladder the type goes:
//!
//! ```ignore
//! peerline_contract::contract!(
//!     "contracts/coagent.runtime.v1.json",
//!     imports { "coagent.rpc/1" => ::coagent_rpc },
//!     supports {
//!         SessionInfo: none,  // not even PartialEq
//!         EventMeta: copy,    // PartialEq + Eq + Copy
//!     },
//! );
//! ```
//!
//! Four rungs, each strictly stronger than the last: `none`, `partial`
//! (`PartialEq`), `total` (`PartialEq + Eq`), `copy` (`PartialEq + Eq +
//! Copy`). A type nobody mentions is assumed `total`, which is what the
//! overwhelming majority of wire shapes are — and when the assumption is
//! wrong the generated derive fails to compile, naming the trait, rather than
//! silently producing a type that compares unequal values as equal.
//!
//! `copy` is not assumed, because assuming it wrongly also fails to compile
//! but assuming it *absently* costs only ergonomics. And `supports` may only
//! speak about imported types: a local shape's rung is computed from the
//! manifest, and letting the call site overrule it would let the Rust
//! contradict the contract.
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
use proc_macro2::TokenStream as TokenStream2;
use quote::ToTokens;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Token, braced};

/// Expand a contract manifest into Rust.
///
/// Takes the path to the manifest, relative to the directory holding the
/// invoking crate's `Cargo.toml` (an absolute path is taken as-is).
/// Crate-relative rather than file-relative because a manifest is a property
/// of the crate, not of whichever module happened to invoke the macro.
///
/// Two optional clauses follow it, `imports` and `supports`; see the crate
/// docs for what each is for and why neither is in the manifest.
#[proc_macro]
pub fn contract(input: TokenStream) -> TokenStream {
    let invocation = syn::parse_macro_input!(input as Invocation);
    match expand(&invocation) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// One `contract!` call: the manifest, plus the two things about *this*
/// target that the manifest deliberately does not say.
struct Invocation {
    manifest: LitStr,
    imports: Vec<(LitStr, TokenStream2)>,
    supports: Vec<(Ident, generate::Support)>,
}

impl Parse for Invocation {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let manifest: LitStr = input.parse()?;
        let mut imports = Vec::new();
        let mut supports = Vec::new();
        let mut seen_imports = false;
        let mut seen_supports = false;
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let keyword: Ident = input.parse()?;
            match keyword.to_string().as_str() {
                "imports" => {
                    if seen_imports {
                        return Err(syn::Error::new(
                            keyword.span(),
                            "`imports` is given twice; one clause carries every mapping",
                        ));
                    }
                    seen_imports = true;
                    imports = parse_imports(input)?;
                }
                "supports" => {
                    if seen_supports {
                        return Err(syn::Error::new(
                            keyword.span(),
                            "`supports` is given twice; one clause carries every type",
                        ));
                    }
                    seen_supports = true;
                    supports = parse_supports(input)?;
                }
                other => {
                    return Err(syn::Error::new(
                        keyword.span(),
                        format!(
                            "`{other}` is not a clause of `contract!`; the two are `imports` and \
                             `supports`"
                        ),
                    ));
                }
            }
        }
        Ok(Self {
            manifest,
            imports,
            supports,
        })
    }
}

/// `imports { "coagent.rpc/1" => ::coagent_rpc, … }`
fn parse_imports(input: ParseStream) -> syn::Result<Vec<(LitStr, TokenStream2)>> {
    let body;
    braced!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        let id: LitStr = body.parse()?;
        body.parse::<Token![=>]>()?;
        out.push((id, parse_rust_path(&body)?));
        if body.is_empty() {
            break;
        }
        body.parse::<Token![,]>()?;
    }
    Ok(out)
}

/// A `::`-separated path, taken as tokens.
///
/// Hand-parsed rather than reached for through `syn::Path`, which lives
/// behind a feature this crate does not otherwise need: what is wanted here
/// is a module prefix, and anything richer would be something the caller
/// could write and this macro could not use.
fn parse_rust_path(input: ParseStream) -> syn::Result<TokenStream2> {
    let mut tokens = TokenStream2::new();
    if input.peek(Token![::]) {
        input.parse::<Token![::]>()?.to_tokens(&mut tokens);
    }
    // `parse_any`, so that `crate`, `self` and `super` can start a path:
    // they are the ones a call site in the crate being generated into
    // reaches for most.
    Ident::parse_any(input)?.to_tokens(&mut tokens);
    while input.peek(Token![::]) {
        input.parse::<Token![::]>()?.to_tokens(&mut tokens);
        Ident::parse_any(input)?.to_tokens(&mut tokens);
    }
    Ok(tokens)
}

/// `supports { SessionInfo: none, EventMeta: copy, … }`
fn parse_supports(input: ParseStream) -> syn::Result<Vec<(Ident, generate::Support)>> {
    let body;
    braced!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        let name: Ident = body.parse()?;
        body.parse::<Token![:]>()?;
        let rung: Ident = body.parse()?;
        let support = match rung.to_string().as_str() {
            "none" => generate::Support::None,
            "partial" => generate::Support::Partial,
            "total" => generate::Support::Total,
            "copy" => generate::Support::Copy,
            other => {
                return Err(syn::Error::new(
                    rung.span(),
                    format!(
                        "`{other}` is not a support level; the four are `none`, `partial`, \
                         `total` and `copy`"
                    ),
                ));
            }
        };
        out.push((name, support));
        if body.is_empty() {
            break;
        }
        body.parse::<Token![,]>()?;
    }
    Ok(out)
}

fn expand(invocation: &Invocation) -> syn::Result<TokenStream2> {
    let literal = &invocation.manifest;
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

    let bindings =
        generate::Bindings::resolve(&manifest, &invocation.imports, &invocation.supports)?;
    generate::contract(&manifest, &bindings, &path, span)
}
