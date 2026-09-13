//! TypeScript, generated from a contract manifest.
//!
//! A **contract manifest** is a language-neutral JSON file describing one
//! peerline contract completely enough to implement it in any language; the
//! format is `peerline-contract-spec`. This crate reads one and writes
//! TypeScript: interfaces for its structs, string-literal unions for its
//! enums, discriminated unions for its unions, and the manifest's own prose as
//! doc comments on every one of them.
//!
//! ```no_run
//! use peerline_contract_spec::Directory;
//! use peerline_contract_ts::{Options, emit_directory};
//!
//! let dir = Directory::load(std::path::Path::new("contracts"))?;
//! for (path, text) in emit_directory(&dir, &Options::default()) {
//!     std::fs::write(std::path::Path::new("bindings").join(path), text)?;
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Why this exists: `manifest → TS`, not `manifest → Rust mirror → ts-rs`
//!
//! There has been a way to get TypeScript out of a manifest for a while, and
//! it goes the long way round. `peerline_contract::contract!` expands the
//! manifest into Rust; a `typescript { … }` clause marks some of those Rust
//! types with `#[derive(ts_rs::TS)]`; a test in a dedicated crate links
//! `ts_rs`, runs `TS::export_all` into a directory, and the result is
//! committed. The route works — it is how the bindings downstream exist today
//! — and it has three costs that are properties of the route rather than of
//! anybody's implementation.
//!
//! **It reads the mirror, not the contract.** `ts_rs` is looking at Rust, and
//! Rust has already thrown things away. The manifest's `i53` — an integer
//! every target holds exactly — becomes a Rust `i64`, which `ts_rs` faithfully
//! reports as `bigint`, which is wrong. The manifest's *absent* and *null* are
//! two states, and a Rust `Option` is one. So the macros crate patches each
//! loss back with a `#[ts(type = "number")]` or a `#[ts(optional)]`, computed
//! from the manifest it still has in hand. Every such attribute is the mirror
//! step being undone. Reading the manifest directly, there is nothing to undo.
//!
//! **It cannot carry the prose.** Every description in a manifest is required,
//! and the format's stated reason is that the reasoning belongs in the shared
//! file so that "a generator emits it as comments in its own target". Through
//! the mirror, a description becomes a Rust doc comment and then has to
//! survive a second generator to reach TypeScript. Here it is one hop, and
//! every struct, field, enum value and union arm carries it.
//!
//! **It costs a Rust build to produce TypeScript.** The existing route needs
//! the manifest expanded into a crate, that crate compiled, `ts_rs` linked,
//! and a test run. A `.ts` file is a function of a `.json` file and nothing
//! else, and this crate's binary is that function.
//!
//! Doing it this way is not a new idea in this repository — it is the
//! sanctioned follow-on. The cluster repo's decision record says it plainly:
//! nothing about generating from a manifest is Rust-only, a TypeScript or
//! Python generator reading the same file would produce that language's types
//! checked against the same file the Rust is drift-tested against, and the
//! objections to the larger Rust migration "do not reach the other targets at
//! all".
//!
//! # What stays
//!
//! **The `typescript { … }` clause keeps working.** Nothing here removes it,
//! deprecates it or changes it. A downstream that generates bindings through
//! `ts_rs` today goes on doing so until it chooses to swap, and the swap is a
//! build-step change rather than a code change — which is why the output
//! layout and naming here follow what that route already produces. The two can
//! run side by side against one contracts directory; `--check` against a
//! committed tree is how you find out whether they agree.
//!
//! Nor does this crate touch `peerline-contract-macros`, which owns that
//! clause. The scalar table here is a second copy of a column that lives
//! there, kept in step by hand and by a cross-check test; `scalar.rs` explains
//! the arrangement and why it cannot be a shared dependency.
//!
//! # The mapping
//!
//! ## Scalars
//!
//! | manifest | TypeScript | why |
//! |---|---|---|
//! | `string` | `string` | |
//! | `bool` | `boolean` | |
//! | `i32`, `u32` | `number` | inside a double's exact range by construction |
//! | `i53`, `u53` | `number` | ±(2^53 − 1) *is* a double's exact range — that is the whole content of the keyword |
//! | `i64`, `u64` | `bigint` | may exceed it; see [`WideIntegers`] |
//! | `f64` | `number` | |
//! | `path` | `string` | a path is a string on the wire; what `path` adds is *where it is resolved*, which the prose states and no type expresses |
//! | `json` | `unknown` | uninterpreted. `unknown` makes a consumer narrow before use; `any` would say the opposite |
//!
//! `bigint` is the honest answer for the wide pair and it is deliberately a
//! little inconvenient: `JSON.parse` does not produce one, so a consumer
//! holding an `i64` has to reach for a reviver, and the type is what tells
//! them at the keyboard rather than in production. [`WideIntegers::Number`]
//! opts out for a consumer that has checked its own data; the better fix is
//! for the manifest to say `i53`, and then every target learns it.
//!
//! ## Structs
//!
//! `export type X = { … }`, not `interface X { … }`. Three reasons, in order
//! of weight. A struct that flattens another *must* be an intersection, and an
//! `interface` that `extends` would be a second spelling for half the cases —
//! one form for every struct reads better than two. An `interface` also
//! participates in declaration merging, so a consumer can silently add fields
//! to a generated contract type, which is exactly the thing a generated file
//! should not permit. And it is what the existing bindings already are.
//!
//! A struct with no fields is `Record<string, never>`, not `{}` — in
//! TypeScript `{}` means "anything but null and undefined", which is the
//! opposite of a shape with no keys.
//!
//! ## The three field states
//!
//! The format distinguishes *required*, *absent* and *null*, and the
//! distinction is the reason it has three keys rather than two.
//!
//! - **required** → `key: T`.
//! - **optional** → `key?: T`, and never `T | null`. A writer omits the key
//!   rather than writing null; those are different bytes, and the manifest
//!   says which.
//! - **nullable** → `key: T | null`, with no `?`. The key is always written.
//!   The format also says a nullable field *accepts* an absent key on the way
//!   in — a decoder that tolerates less than it emits helps nobody — but that
//!   is a tolerance, not a permission: a `key?: T | null` would let a
//!   TypeScript caller omit a key the contract says is always written. The
//!   tolerance is stated here, once, rather than stamped above every nullable
//!   field in every generated file — the prose a reader finds there should be
//!   the contract author's, not the generator's boilerplate.
//! - **default** → `key?: T`, with the default value in the doc comment. This
//!   is the one place the emitter departs from what the `ts_rs` route
//!   produces, and the reasoning is below.
//!
//! ### Why a defaulted field is `key?:`
//!
//! Reason from what serde does, because that is what is on the wire. A
//! `#[serde(default = …)]` field with no `skip_serializing_if` is **absent-
//! tolerant on the way in and always written on the way out**. TypeScript has
//! one type for both directions, so one of the two readings has to win.
//!
//! The wire-in reading wins, for two reasons. A default's natural home is an
//! args struct — the manifest's own example is a listing whose "empty object
//! is a complete request" — and args are what a TypeScript consumer
//! *constructs*. And the two ways of being wrong are not symmetric: emitting
//! `key?: T` for a value a producer always writes gives a reader a
//! `T | undefined` where a `T` was guaranteed, a widening that costs a
//! needless check; emitting `key: T` for a caller forces them to restate a
//! value the contract already supplies, which is a falsehood the compiler
//! enforces. Pick the direction in which being wrong is safe.
//!
//! The `ts_rs` route produces `key: T` here — a defaulted field is neither
//! optional nor nullable, so nothing marks it. [`DefaultFields::Required`]
//! reproduces that for a downstream that wants a byte-identical swap first
//! and the better types afterwards.
//!
//! ## Enums
//!
//! A string-literal union of **wire** spellings:
//!
//! ```ts
//! export type Backend =
//!   /** Two words on the wire, hyphenated. */
//!   | "claude-code"
//!   /** One word, so the name and the wire value are the same. */
//!   | "codex";
//! ```
//!
//! Not a TypeScript `enum`: a `enum` is a runtime value, it is not assignable
//! from the string a `JSON.parse` produced, and `const enum` has its own
//! build-time problems. A literal union is a pure type, narrows in a `switch`,
//! and is what a decoded wire value already is.
//!
//! The variant `name` is not emitted anywhere. It is the cross-generator
//! identifier — what Rust spells `GoneAway` and Python might spell
//! `GONE_AWAY` — and TypeScript's member here *is* the wire string, so there
//! is nothing for the identifier to name. Its prose is emitted, above the
//! literal.
//!
//! ## Unions
//!
//! **Internally tagged** — a discriminated union on the tag key, which is what
//! TypeScript narrows on:
//!
//! ```ts
//! export type Item =
//!   | ({ item: "event" } & Event)
//!   | {
//!       item: "gap";
//!       lost: bigint;
//!     };
//! ```
//!
//! An arm carrying a whole shape becomes an intersection because that is what
//! the wire is: the payload's keys sit beside the tag. An arm spelling its own
//! fields becomes one object with the tag first.
//!
//! **Externally tagged** — one key, which is the arm name:
//!
//! ```ts
//! export type KindFilter =
//!   | { only: Array<Backend> }
//!   | { except: Array<Backend> };
//! ```
//!
//! An externally tagged arm with neither a payload nor fields is a bare
//! string, because serde writes a unit variant as its name rather than as an
//! object with a null value.
//!
//! ## Flatten
//!
//! An intersection. A flattened field's keys are written beside its siblings,
//! so the enclosing object really does have both sets of keys, and `A & B` is
//! how TypeScript spells that. A struct whose fields are *all* flattened is
//! the intersection alone, with no `{}` in front of it.
//!
//! ## Arrays
//!
//! `Array<T>`, not `T[]`. Identical to the compiler; the generic form is what
//! the existing bindings use and it stays readable when the element is a
//! union.
//!
//! ## Imports
//!
//! TypeScript `import type` between the generated files, resolved from the
//! manifest's own `imports` — which name a contract and a version and never a
//! path, so the file name is this generator's business and the mapping is the
//! same one `Directory::load` enforces: `<contract>.v<version>`.
//!
//! `import type` rather than `import`: every name here is a type, and the
//! type-only form is erased at compile time, so a generated file adds nothing
//! to a bundle.
//!
//! # Layout
//!
//! [`Layout::Type`] is one file per named type, which is what `ts_rs` produces
//! and therefore the default. [`Layout::Manifest`] is one file per manifest,
//! named after it. Either can be accompanied by an `index.ts` barrel; see
//! [`Options::barrel`].
//!
//! # What this crate does not emit
//!
//! **Ops and error codes.** A manifest declares them and the Rust generator
//! emits constants for both, and there is nothing hard about doing the same
//! here. It is left out because the thing that makes this generator adoptable
//! is that its output can be diffed against what a downstream has already
//! committed, and what they have committed is shapes. A file that also carried
//! `OP_THREAD_START` would differ from the tree it is meant to replace for a
//! reason that has nothing to do with whether the mapping is right. It is the
//! obvious next addition once a downstream has swapped.
//!
//! **A client.** Generating call stubs means deciding what a peerline
//! TypeScript client looks like, which is a question about `peerline-ts` and
//! not about this format.
//!
//! **Formatting by anybody else.** The output is written the way Prettier's
//! defaults would write it — two-space indents, double quotes, semicolons
//! between object-type members, leading `|` on a multi-line union, a trailing
//! newline — so that a repository which runs Prettier over its generated tree
//! finds nothing to change and `--check` stays quiet.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod doc;
mod render;
mod scalar;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use peerline_contract_spec::{Directory, Manifest};

pub use scalar::WideIntegers;

/// Whether a defaulted field is emitted as absent-able.
///
/// See the crate docs for why [`Optional`](Self::Optional) is the default and
/// what [`Required`](Self::Required) is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DefaultFields {
    /// `key?: T`, with the default value in the doc comment. The wire-in
    /// reading, and the one in which being wrong is safe.
    #[default]
    Optional,
    /// `key: T`. What the `ts_rs` route produces, for a downstream that wants
    /// the swap to be byte-identical before it is an improvement.
    Required,
}

/// One file per type, or one per manifest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    /// `Session.ts`, `Backend.ts`, … — one file per named type, importing its
    /// siblings by name. What `ts_rs` produces, and therefore the default, so
    /// that a downstream swapping generators diffs contents rather than a
    /// reorganised tree.
    #[default]
    Type,
    /// `demo.wire.v1.ts` — one file per manifest, holding every shape it
    /// declares and importing only across manifests. Fewer files, fewer
    /// imports, and the file name is the manifest's own.
    Manifest,
}

/// What to generate and how.
///
/// Every field has a default that is either the compatible answer or the
/// honest one, and where those differ the crate docs say which is which.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `bigint` or `number` for `i64` and `u64`.
    pub wide_integers: WideIntegers,
    /// Whether a defaulted field gets a `?`.
    pub defaults: DefaultFields,
    /// One file per type or one per manifest.
    pub layout: Layout,
    /// Also emit an `index.ts` re-exporting every generated type.
    ///
    /// Off by default: a barrel is a convenience for a package that wants one
    /// import specifier, and a file the generator did not previously write is
    /// a diff a downstream did not ask for.
    pub barrel: bool,
}

/// One manifest as a single TypeScript module.
///
/// This is [`Layout::Manifest`]'s unit, and it needs no [`Directory`]: a
/// manifest declares the contracts it imports from, and the file name of a
/// manifest is a function of what it declares, so the `import type` lines are
/// derivable from this file alone. Use [`emit_directory`] to write a whole
/// tree, or to use [`Layout::Type`].
pub fn emit(manifest: &Manifest, opts: &Options) -> String {
    let decls = render::decls(&manifest.types, opts);
    let imported = manifest.imported_types();

    // Only names that live in another file need an import, and under this
    // layout that is exactly the imported ones.
    let mut needed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for decl in &decls {
        for name in &decl.refers_to {
            if let Some(import) = imported.get(name.as_str()) {
                needed
                    .entry(module_for(&import.contract, import.version))
                    .or_default()
                    .insert(name.clone());
            }
        }
    }

    let mut out = header(&manifest.id(), &file_for(manifest));
    out.push_str(&doc::block(&manifest.description, 0));
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
    for (module, names) in &needed {
        out.push_str(&import_line(names, module));
    }
    if !needed.is_empty() {
        out.push('\n');
    }
    for (i, decl) in decls.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&decl.text);
    }
    out
}

/// Every manifest in a directory, as the files a generator writes.
///
/// Keyed by path relative to the output directory, so a caller joins them onto
/// wherever the bindings live. Sorted, because a build that writes them in a
/// different order every run is a build whose output a `--check` cannot trust.
///
/// The [`Directory`] rather than a bag of manifests because that is the unit a
/// contract is written in: it is what has already checked that every import
/// resolves and that no two contracts claim one error code, which is what lets
/// this function assume every name it meets has somewhere to come from.
pub fn emit_directory(dir: &Directory, opts: &Options) -> BTreeMap<PathBuf, String> {
    let mut files: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut exports: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    match opts.layout {
        Layout::Manifest => {
            for manifest in dir.manifests() {
                let module = file_for(manifest);
                let names: BTreeSet<String> = manifest
                    .types
                    .iter()
                    .map(|t| t.name().to_string())
                    .collect();
                if !names.is_empty() {
                    exports.insert(module_for(&manifest.contract, manifest.version), names);
                }
                files.insert(PathBuf::from(format!("{module}.ts")), emit(manifest, opts));
            }
        }
        Layout::Type => {
            for manifest in dir.manifests() {
                for decl in render::decls(&manifest.types, opts) {
                    // Every reference lives in its own file under this
                    // layout, local or imported alike, so the import path is
                    // the name and the manifest's own `imports` are not
                    // consulted at all.
                    let mut out = header(&manifest.id(), &decl.name);
                    for name in &decl.refers_to {
                        out.push_str(&import_line(
                            &BTreeSet::from([name.clone()]),
                            &module_for_type(name),
                        ));
                    }
                    if !decl.refers_to.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&decl.text);
                    exports
                        .entry(module_for_type(&decl.name))
                        .or_default()
                        .insert(decl.name.clone());
                    files.insert(PathBuf::from(format!("{}.ts", decl.name)), out);
                }
            }
        }
    }

    if opts.barrel {
        let mut out = String::from(
            "// Generated by peerline-contract-ts. Do not edit.\n\
             //\n\
             // Every shape the contracts directory declares, in one specifier.\n\n",
        );
        for (module, names) in &exports {
            out.push_str(&import_line_kw("export type", names, module));
        }
        files.insert(PathBuf::from("index.ts"), out);
    }
    files
}

/// The banner every generated file carries.
///
/// It names the manifest rather than this crate alone, because the question a
/// reader who has found a wrong type actually has is "where do I change it",
/// and the answer is a `.json` file they can be pointed at by name.
fn header(id: &str, _file: &str) -> String {
    let (contract, version) = id.rsplit_once('/').unwrap_or((id, "1"));
    format!(
        "// Generated by peerline-contract-ts from the `{id}` contract manifest.\n\
         // Do not edit: change `{contract}.v{version}.json` and regenerate.\n\n"
    )
}

fn import_line(names: &BTreeSet<String>, module: &str) -> String {
    import_line_kw("import type", names, module)
}

fn import_line_kw(keyword: &str, names: &BTreeSet<String>, module: &str) -> String {
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    format!("{keyword} {{ {} }} from \"{module}\";\n", names.join(", "))
}

/// The module specifier for another manifest's file. Extensionless, which is
/// what the existing bindings use and what TypeScript's classic resolution
/// wants; a consumer on `NodeNext` rewrites these, as it already does for the
/// tree this replaces.
fn module_for(contract: &str, version: u32) -> String {
    format!("./{contract}.v{version}")
}

fn module_for_type(name: &str) -> String {
    format!("./{name}")
}

/// The file stem a manifest's module gets, mirroring the `<contract>.v<n>.json`
/// name `Directory::load` already requires of the manifest itself.
fn file_for(manifest: &Manifest) -> String {
    format!("{}.v{}", manifest.contract, manifest.version)
}
