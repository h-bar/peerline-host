//! The `contract!` macro: a contract manifest, compiled.
//!
//! Reach it through `peerline-contract`, which re-exports it, so that a crate
//! implementing a contract names one dependency rather than three.
//!
//! ```ignore
//! peerline_contract::contract!("codex.server/1" in "contracts");
//! ```
//!
//! # The unit is the directory, not the file
//!
//! A call site names the **contracts directory** and which contract it wants
//! out of it, because the directory is what a contract is written in: the
//! manifests in one reference each other's shapes and share one error-code
//! space, so a file on its own can be valid and the set of them still wrong.
//!
//! Every manifest in the directory is therefore read and checked together,
//! and all of it lands on the call site as a compile error: a file that does
//! not parse, a file whose name disagrees with the contract it declares, an
//! import naming a type the other manifest does not declare, two contracts
//! picking the same error code. Before, those were a test in whichever crate
//! remembered to write one.
//!
//! It is also what lets an imported shape be read rather than described. See
//! the derives section below.
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
//!     "coagent.runtime/1" in "contracts",
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
//! ## What this clause is not
//!
//! It carries no correctness. Whether `coagent.rpc/1` exists, whether it
//! declares the types the importing manifest lists, and what those types are
//! shaped like are all answered by the directory, which the macro has read.
//! What is left is one Rust fact — *which module holds that vocabulary's
//! expansion* — and that is the same category as `typescript { … }`: a thing
//! about this build that no other implementor of the same contract would
//! write down.
//!
//! An earlier revision also asked the call site for `supports { … }`, a rung
//! per imported type saying how far up the comparison ladder its Rust
//! supplier went. That existed only because the macro could not read the
//! manifest it was importing from; now that it can, the rung is computed —
//! see the next section.
//!
//! # Derives, and why they are not in the manifest
//!
//! The comparison and copy derives on a generated type are read off the
//! *shape*: a record of exact scalars can carry `Eq`, one holding an `f64`
//! cannot, and neither fact is something the manifest states — they follow
//! from what it already says, which is why they are not keys. A manifest that
//! listed Rust derives would be a manifest with a Rust-shaped hole in it.
//!
//! An **imported** shape is read the same way, out of the manifest that
//! declares it. That is the direct consequence of the unit being the
//! directory: the fields are in another file, and the other file is open.
//!
//! ## When the Rust supplier is hand-written
//!
//! `imports` may point at a module whose types were written by hand rather
//! than generated, and a hand-written type is free to lack `PartialEq` that
//! the manifest's shape implies. Nothing here detects that, and nothing
//! tries: the generated derive fails to compile, naming the trait, at the
//! type that tried to compare it. That is the same "wrong and loud beats
//! wrong and silent" position the rungs were justified by, and it is the
//! half of it that was doing the work — a rung stated at the call site could
//! be *wrong in the safe direction* and then nothing said anything, which is
//! wrong and quiet.
//!
//! # `typescript`, and why the derive is not in the manifest
//!
//! Some contracts are read by a browser. `ts_rs` turns a Rust type into a
//! TypeScript declaration, and a crate that publishes bindings needs the
//! derive on the shapes it publishes. Which shapes those are is a fact about
//! *this* target — a Python generator has no opinion on it, and a service
//! that speaks the same contract over a socket does not want `ts_rs` linked
//! at all — so it is said at the call site, exactly like `imports`:
//!
//! ```ignore
//! peerline_contract::contract!(
//!     "coagent.rpc/1" in "contracts",
//!     typescript { BackendKind, CoagentEventKind, SessionInfo },
//! );
//! ```
//!
//! Each named shape gains `#[derive(::ts_rs::TS)]`, the wire spelling of
//! each of its variants as an explicit `#[ts(rename = …)]`, and — this is
//! the part a human otherwise has to remember — the TypeScript type of every
//! field whose Rust spelling has lost something the manifest knows:
//!
//! - A field declared `i53` or `u53` generates an `i64` or a `u64`, which
//!   `ts_rs` maps to `bigint`. The manifest says the value is exact in every
//!   target, so the expansion writes `#[ts(type = "number")]`. A field
//!   declared `i64` or `u64` gets no such attribute, because for those the
//!   `bigint` is the truth and the warning.
//! - A field declared `json` generates a `serde_json::Value`, which `ts_rs`
//!   maps to a named `JsonValue` import. The manifest says the value is
//!   passed through uninterpreted, which TypeScript spells `unknown`.
//! - A field the manifest marks absent-able gets `#[ts(optional)]`, so it is
//!   `key?: T` rather than `key: T | null`. The manifest distinguishes the
//!   two states; this is where the distinction reaches the browser.
//!
//! Before this clause those three lived as per-field attributes in a
//! hand-written Rust crate — a portability fact about the *value*, written
//! in one language, where no other implementor could see it. `i53` moves the
//! fact into the contract; this clause is what reads it.
//!
//! ## `ts_rs` stays optional, by construction rather than by feature
//!
//! Neither this crate nor `peerline-contract` depends on `ts_rs`. The derive
//! is emitted as the path `::ts_rs::TS`, which resolves in the crate the
//! macro expanded into — so a crate that writes the clause declares `ts-rs`
//! in its own `Cargo.toml` like any other dependency, and a crate that does
//! not write it emits no mention of `ts_rs` and links nothing.
//!
//! ## What it does not carry
//!
//! No output path. `ts_rs`'s own `export_to` names a file relative to an
//! output directory, and the mechanism that actually writes bindings here
//! passes that directory at run time (`Config::with_out_dir`) and exports
//! each root with `TS::export_all`. A path in the clause would be a second
//! place for the destination to be decided, and the first one already wins.
//! If a target ever needs bundling into a shared file, that is the clause to
//! extend.
//!
//! An imported shape may not be named: its Rust type belongs to the crate
//! that expands the manifest it came from, and that crate's call site is
//! where its TypeScript is asked for. A shape that *holds* an unexported
//! local shape is rejected too, because `ts_rs` derives through members and
//! the failure would otherwise land somewhere that does not mention the
//! clause.
//!
//! # A generated set is closed, and no attribute says otherwise
//!
//! A generated enum or union carries no `#[non_exhaustive]`. A reader who
//! expects one on a versioned wire type should read this paragraph as the
//! answer rather than as an oversight.
//!
//! The two halves of the generated code have to agree. `#[non_exhaustive]`
//! buys source-level tolerance for a variant the reader has not heard of —
//! but the expansion emits no catch-all variant, so serde **rejects** an
//! unknown value at the decode boundary, and a manifest's own prose is
//! usually explicit that this is intended (`coagent.rpc/1`'s `BackendKind`
//! says "an absent or unrecognised tag is a decoding failure at the
//! boundary"). So a Rust program can never *hold* a value outside its
//! compiled set unless it was built against the newer manifest — in which
//! case it has the variant, and the tolerance was never needed.
//!
//! What the attribute does buy is a cost. The claim above — the types **are**
//! the manifest, so a manifest edit either changes the Rust or fails to
//! compile — holds only inside the crate that expanded it, because
//! `non_exhaustive` is a no-op there. The moment generated types cross a
//! crate boundary, which is what a **vocabulary** is for, every exhaustive
//! match in every consumer would have to grow a wildcard arm, and a contract
//! that grew a value would stop being a compile error and become a runtime
//! one — or, for a store that encodes a variant as a column value, a silently
//! wrong integer in an on-disk format.
//!
//! If openness is ever wanted it belongs in the **manifest**, not here. Not
//! as a manifest key spelled after a Rust attribute — that would be a
//! Rust-shaped hole in a language-neutral file — but as something the format
//! can *say* about a set, in its own terms, the way it already says `optional`
//! and `nullable`. And not as a call-site clause either: openness is a
//! property of the contract, not of this build, and a call site is only ever
//! asked about this build. When the format can say it, the generator emits
//! the catch-all variant **and** the attribute together, so the wire and the
//! Rust go on agreeing.
//!
//! # Why a manifest edit forces a rebuild
//!
//! A proc macro that reads a file does not, by itself, tell cargo that the
//! file is an input: nothing in the crate's source changed, so the crate is
//! not rebuilt, and the compiled types go on describing a manifest that no
//! longer exists. That is the worst failure available to a design whose whole
//! claim is that the two cannot drift.
//!
//! So the expansion ends with an `include_bytes!` bound to `const _` for
//! **every file in the directory**, not just the one asked for. The compiler
//! records each as a source input in the crate's dependency info, cargo reads
//! that, and touching any manifest rebuilds the crate. The constants are
//! never read — they exist to be dependency edges.
//!
//! Every file, because every file was read: a vocabulary supplies an imported
//! shape's derives, and any manifest at all can collide on an error code, so
//! an edit anywhere in the directory can change or invalidate this expansion.
//! Over-rebuilding is the safe direction and the only one available — the
//! alternative is a crate that goes on trusting a check made against a file
//! that has since changed.
//!
//! What this does **not** catch is a manifest being *added*. A dependency
//! edge is a file, and a file that did not exist when the crate was built has
//! no edge to record; there is no stable way to make cargo watch a directory
//! for new entries. So a manifest added with nothing else touched is first
//! checked on the next build that happens for another reason. A new manifest
//! arrives with the call site that expands it, which is a source edit, so
//! this has no reach in practice — but it is the one gap, and it is here
//! rather than left to be discovered.

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
/// Takes the contract wanted and the directory holding it —
/// `"coagent.runtime/1" in "contracts"` — with the path relative to the
/// directory holding the invoking crate's `Cargo.toml` (an absolute path is
/// taken as-is). Crate-relative rather than file-relative because a
/// contracts directory is a property of the crate, not of whichever module
/// happened to invoke the macro.
///
/// Two optional clauses follow it — `imports` and `typescript`; see the
/// crate docs for what each is for and why neither is in the manifest.
#[proc_macro]
pub fn contract(input: TokenStream) -> TokenStream {
    let invocation = syn::parse_macro_input!(input as Invocation);
    match expand(&invocation) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// One `contract!` call: which contract, out of which directory, plus the
/// two things about *this* target that the manifest deliberately does not
/// say.
struct Invocation {
    wanted: LitStr,
    directory: LitStr,
    imports: Vec<(LitStr, TokenStream2)>,
    typescript: Vec<Ident>,
}

impl Parse for Invocation {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let wanted: LitStr = input.parse()?;
        input.parse::<Token![in]>()?;
        let directory: LitStr = input.parse()?;
        let mut imports = Vec::new();
        let mut typescript = Vec::new();
        let mut seen_imports = false;
        let mut seen_typescript = false;
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
                "typescript" => {
                    if seen_typescript {
                        return Err(syn::Error::new(
                            keyword.span(),
                            "`typescript` is given twice; one clause carries every shape",
                        ));
                    }
                    seen_typescript = true;
                    typescript = parse_typescript(input)?;
                }
                other => {
                    return Err(syn::Error::new(
                        keyword.span(),
                        format!(
                            "`{other}` is not a clause of `contract!`; the two are `imports` \
                             and `typescript`"
                        ),
                    ));
                }
            }
        }
        Ok(Self {
            wanted,
            directory,
            imports,
            typescript,
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

/// `typescript { SessionInfo, BackendKind, … }`
fn parse_typescript(input: ParseStream) -> syn::Result<Vec<Ident>> {
    let body;
    braced!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        out.push(body.parse()?);
        if body.is_empty() {
            break;
        }
        body.parse::<Token![,]>()?;
    }
    Ok(out)
}

fn expand(invocation: &Invocation) -> syn::Result<TokenStream2> {
    let span = invocation.directory.span();
    let given = PathBuf::from(invocation.directory.value());
    let path = if given.is_absolute() {
        given
    } else {
        let root = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
            syn::Error::new(
                span,
                "CARGO_MANIFEST_DIR is not set, so a relative directory cannot be resolved; \
                 pass an absolute path",
            )
        })?;
        PathBuf::from(root).join(given)
    };

    let directory = peerline_contract_spec::Directory::load(&path)
        .map_err(|err| syn::Error::new(span, format!("`{}`: {err}", path.display())))?;

    let span = invocation.wanted.span();
    let wanted = invocation.wanted.value();
    let (contract, version) = split_id(&wanted, span)?;
    let manifest = directory.get(contract, version).ok_or_else(|| {
        let held: Vec<String> = directory.manifests().iter().map(|m| m.id()).collect();
        syn::Error::new(
            span,
            format!(
                "`{}` holds no `{wanted}`; it holds {}",
                path.display(),
                held.join(", ")
            ),
        )
    })?;

    let bindings =
        generate::Bindings::resolve(manifest, &invocation.imports, &invocation.typescript)?;
    generate::contract(manifest, &directory, &bindings, span)
}

/// `"coagent.runtime/1"` — the same spelling a manifest's own `imports` use,
/// so that a call site and the file it reads name a contract the same way.
fn split_id(id: &str, span: proc_macro2::Span) -> syn::Result<(&str, u32)> {
    let bad = || {
        syn::Error::new(
            span,
            format!(
                "`{id}` does not name a contract; write it as a manifest's own imports do, \
                 `<contract>/<major version>`, e.g. `coagent.rpc/1`"
            ),
        )
    };
    let (contract, version) = id.rsplit_once('/').ok_or_else(bad)?;
    let version: u32 = version.parse().map_err(|_| bad())?;
    Ok((contract, version))
}
