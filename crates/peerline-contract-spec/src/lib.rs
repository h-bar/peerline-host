//! The contract manifest format.
//!
//! A **contract manifest** is a JSON file that describes one peerline
//! contract completely enough to implement it in any language: its name and
//! major version, the ops it defines, whether each op is unary or a stream,
//! the shapes of their arguments, results and stream items, the error codes a
//! producer may raise, and — in the same file — the prose explaining why each
//! of those is what it is.
//!
//! This crate parses and validates one. It does not generate anything;
//! `peerline-contract-macros` turns a validated manifest into Rust, and a
//! conformance checker in any other language reads the same file.
//!
//! # Why the manifest is the source, and not the Rust
//!
//! Generating a spec *out of* Rust keeps Rust as the authority, and that has a
//! known failure: Rust can express things a target cannot. peerline's own
//! `vectors.json` already carries entries marked `tsOnly` because an id past
//! 2^53 is a legal `u64` and outside JavaScript's safe-integer range. A
//! language-neutral file has no such bias, and every implementation —
//! including Rust's — is checked against *it*.
//!
//! # This is not [`crate`]'s only meaning of "manifest"
//!
//! `peerline_contract::manifest` carries a different thing with the same word:
//! the envelope a *binary* writes when it is probed, listing the contracts
//! each of its mounted services produces and consumes. That is a statement
//! about one process. This is a statement about one contract, independent of
//! any process, and it is spelled `spec` where the two would otherwise
//! collide.
//!
//! # Shape
//!
//! ```jsonc
//! {
//!   "contract": "codex.server",
//!   "version": 1,
//!   "description": ["Why this contract exists."],
//!   "types": [
//!     { "form": "struct", "name": "StopArgs", "description": ["..."],
//!       "fields": [ { "name": "thread_id", "type": "string",
//!                     "description": ["Which thread to release."] } ] }
//!   ],
//!   "errors": [
//!     { "name": "codex_server", "code": 3101, "data": "Fault",
//!       "description": ["A producer-side failure."] }
//!   ],
//!   "ops": [
//!     { "name": "codex.server.thread.stop", "kind": "unary",
//!       "description": ["..."], "args": "StopArgs", "result": null,
//!       "errors": ["codex_server"] }
//!   ]
//! }
//! ```
//!
//! # `kind` is load-bearing
//!
//! Unary and stream are different peerline calls. A producer that registers a
//! stream op as unary answers a `call_stream` with a kind mismatch at runtime,
//! which no argument or result schema would have caught. So `kind` is a
//! required field with exactly two values, and the shape a kind implies is
//! enforced here: a unary op has a `result` and no `item`, a stream op has an
//! `item` and no `result`.
//!
//! # Every description is required
//!
//! Roughly half of a hand-written Rust contract crate is prose explaining why
//! each shape is what it is, and that prose is contract documentation — read
//! by whoever implements the contract, in whatever language. It belongs here,
//! written once, and a generator emits it as comments in its own target. A
//! manifest with a shape nobody explained is rejected rather than accepted
//! with a warning, because a warning is how the prose fails to move.
//!
//! The prose is **plain**: no rustdoc intra-doc links, no markdown link
//! syntax. Its readers are not necessarily writing Rust, and a `[`Foo`]`
//! renders as literal noise in every target that is not rustdoc.
//! See [`Prose`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod names;
mod validate;

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

pub use names::{const_name, pascal_case, screaming_case};
pub use validate::Error;

/// One contract, described completely.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The contract name, e.g. `"codex.server"`. The same string a producer
    /// registers as `ContractId::name`, and the required prefix of every op
    /// name in this file.
    pub contract: String,
    /// The major version. One integer, additive within it — a breaking change
    /// is a new number and a new file, never an edit in place.
    pub version: u32,
    /// Why this contract exists and what it deliberately is not. The prose a
    /// generator puts at the top of whatever it emits.
    pub description: Prose,
    /// The named shapes this contract's ops exchange. Every `args`, `result`,
    /// `item` and error `data` either names one of these or is a scalar.
    #[serde(default)]
    pub types: Vec<TypeDef>,
    /// The error codes a producer of this contract may raise, with what each
    /// one means and what it carries. Declared once and referenced by name
    /// from the ops that can raise them, because one code is normally raised
    /// by several ops and two copies of its meaning would disagree.
    #[serde(default)]
    pub errors: Vec<ErrorDef>,
    /// The ops, in whatever order reads best — normally lifecycle order.
    pub ops: Vec<OpDef>,
}

impl Manifest {
    /// Parse and validate a manifest from its JSON bytes.
    ///
    /// Validation is not separable: an unvalidated manifest can name a type
    /// that does not exist or an op whose kind contradicts its shape, and
    /// every consumer of this crate would have to re-check the same things.
    pub fn parse(json: &str) -> Result<Self, Error> {
        let manifest: Self = serde_json::from_str(json).map_err(Error::syntax)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// The named types, keyed by name. Built after validation, so every
    /// reference in the file resolves in here.
    pub fn type_map(&self) -> BTreeMap<&str, &TypeDef> {
        self.types.iter().map(|t| (t.name(), t)).collect()
    }

    /// The declared errors, keyed by name.
    pub fn error_map(&self) -> BTreeMap<&str, &ErrorDef> {
        self.errors.iter().map(|e| (e.name.as_str(), e)).collect()
    }
}

/// Prose, as one string or as a list of lines.
///
/// The list form exists because the interesting prose in a contract is
/// paragraphs, and a JSON string full of `\n` is unreadable in the file that
/// is supposed to be the documentation. The two forms mean the same thing; a
/// string containing newlines is split on them.
///
/// **Plain prose only.** No rustdoc intra-doc links (`[`Foo`]`) and no
/// markdown links (`[text](url)`): a manifest is read by implementors in
/// languages whose doc tooling renders neither, and the validator rejects
/// both so that Rust's rendering conventions cannot leak into the shared file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Prose {
    /// A single line, or several separated by `\n`.
    Line(String),
    /// One entry per line. A blank entry is a paragraph break.
    Lines(Vec<String>),
}

impl Prose {
    /// The prose as individual lines, however it was written.
    pub fn lines(&self) -> Vec<&str> {
        match self {
            Self::Line(s) => s.split('\n').collect(),
            Self::Lines(v) => v.iter().flat_map(|s| s.split('\n')).collect(),
        }
    }

    /// Whether this carries no text at all — the one thing a description may
    /// not be.
    pub fn is_empty(&self) -> bool {
        self.lines().iter().all(|l| l.trim().is_empty())
    }
}

/// A type reference: a scalar keyword, a named type in this manifest, or an
/// array of either.
///
/// Written as a bare string in the common case (`"string"`, `"Fault"`) and as
/// `{ "array": … }` for a list. A bare string that is not one of the nine
/// scalar keywords is a reference to a [`TypeDef`] by name, and the validator
/// rejects one that does not resolve — so a misspelled scalar fails loudly
/// rather than becoming a dangling reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Schema {
    /// A scalar keyword or the name of a [`TypeDef`].
    Named(String),
    /// A list of the inner schema.
    Array {
        /// What the list holds.
        array: Box<Schema>,
    },
}

/// The closed set of scalars a manifest may name.
///
/// Closed deliberately. A contract that could say "some other type" would be
/// a contract nobody can check for portability, and portability is the whole
/// reason the manifest is the source rather than the Rust: the set of scalars
/// is exactly the set whose representation in every target is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scalar {
    /// UTF-8 text.
    String,
    /// `true` / `false`.
    Bool,
    /// A signed 32-bit integer.
    I32,
    /// An unsigned 32-bit integer.
    U32,
    /// A signed 64-bit integer. Beyond JavaScript's safe-integer range: see
    /// the crate docs on `tsOnly`.
    I64,
    /// An unsigned 64-bit integer. Same caveat as [`Self::I64`].
    U64,
    /// A double.
    F64,
    /// A filesystem path, as a string. Distinguished from [`Self::String`]
    /// because it is resolved somewhere, and which machine resolves it is a
    /// fact the prose has to state.
    Path,
    /// Arbitrary JSON, passed through without being interpreted.
    Json,
}

impl Scalar {
    /// The keyword a manifest writes for this scalar.
    pub fn keyword(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Bool => "bool",
            Self::I32 => "i32",
            Self::U32 => "u32",
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::F64 => "f64",
            Self::Path => "path",
            Self::Json => "json",
        }
    }

    /// The scalar a keyword names, if it is one.
    pub fn from_keyword(word: &str) -> Option<Self> {
        Some(match word {
            "string" => Self::String,
            "bool" => Self::Bool,
            "i32" => Self::I32,
            "u32" => Self::U32,
            "i64" => Self::I64,
            "u64" => Self::U64,
            "f64" => Self::F64,
            "path" => Self::Path,
            "json" => Self::Json,
            _ => return None,
        })
    }

    /// Whether two values of this scalar can be compared for exact equality.
    /// `f64` and `json` cannot, which is what decides whether a generated type
    /// can carry a total-equality derive.
    pub fn is_exact(self) -> bool {
        !matches!(self, Self::F64 | Self::Json)
    }
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.keyword())
    }
}

/// One named shape.
///
/// Three forms, because three is what the wire actually uses: a record, a
/// closed set of names, and a tagged union of records. Anything else is
/// expressible as a composition of these, and adding a fourth is a change to
/// every generator at once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum TypeDef {
    /// A JSON object with a fixed set of fields.
    Struct {
        /// The type's name, in the manifest's own spelling.
        name: String,
        /// What this shape is for.
        description: Prose,
        /// Its fields, in wire order.
        fields: Vec<Field>,
    },
    /// A closed set of string values.
    Enum {
        /// The type's name.
        name: String,
        /// What the set is for and why it is closed.
        description: Prose,
        /// The permitted values.
        variants: Vec<EnumVariant>,
    },
    /// A tagged union: a JSON object carrying a discriminator field plus the
    /// fields of the selected variant.
    Union {
        /// The type's name.
        name: String,
        /// What the union is for.
        description: Prose,
        /// The field carrying the discriminator, e.g. `"kind"`.
        tag: String,
        /// The variants, each with its own fields.
        variants: Vec<UnionVariant>,
    },
}

impl TypeDef {
    /// The declared name.
    pub fn name(&self) -> &str {
        match self {
            Self::Struct { name, .. } | Self::Enum { name, .. } | Self::Union { name, .. } => name,
        }
    }

    /// The prose describing it.
    pub fn description(&self) -> &Prose {
        match self {
            Self::Struct { description, .. }
            | Self::Enum { description, .. }
            | Self::Union { description, .. } => description,
        }
    }
}

/// One field of a struct or of a union variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// The wire name, in the manifest's own spelling. This is the JSON key.
    pub name: String,
    /// What the field holds.
    #[serde(rename = "type")]
    pub ty: Schema,
    /// Whether the field may be absent. An absent optional field is not the
    /// same as a present null one in every target, so a generator omits it on
    /// the way out rather than writing `null`.
    #[serde(default)]
    pub optional: bool,
    /// What this field means and why it is here. Required — see the crate
    /// docs.
    pub description: Prose,
}

/// One value of a string enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumVariant {
    /// The value as it appears on the wire.
    pub name: String,
    /// What it means, and what a reader should do differently on seeing it.
    pub description: Prose,
}

/// One arm of a tagged union.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnionVariant {
    /// The discriminator value for this arm.
    pub name: String,
    /// What this arm means.
    pub description: Prose,
    /// The fields this arm carries alongside the discriminator.
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// An error a producer of this contract may raise.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorDef {
    /// The error's name in the manifest's own spelling, e.g.
    /// `"codex_server"`. A generator decorates it in its own convention.
    pub name: String,
    /// The numeric code carried on the wire. A consumer matches on this and
    /// then decodes [`Self::data`], so two contracts in one process must not
    /// pick the same number.
    pub code: i32,
    /// What went wrong, and what the consumer should do about it.
    pub description: Prose,
    /// The shape carried alongside the code, if any — the typed payload a
    /// consumer decodes once it has matched the code.
    #[serde(default)]
    pub data: Option<Schema>,
}

/// Whether an op is a single request/response or a stream of items.
///
/// The one field of this format that no argument or result schema could
/// substitute for: unary and stream are different peerline calls, and a
/// producer that registers the wrong one fails at runtime with a kind
/// mismatch rather than with anything a shape check would have found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    /// One request, one response.
    Unary,
    /// One request, then zero or more items, then a terminal.
    Stream,
}

impl OpKind {
    /// The keyword a manifest writes.
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Unary => "unary",
            Self::Stream => "stream",
        }
    }
}

impl fmt::Display for OpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.keyword())
    }
}

/// One op of the contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpDef {
    /// The full op name as it travels on the wire, e.g.
    /// `"codex.server.thread.start"`. Must begin with the contract name and a
    /// dot, so that two contracts mounted on one peer cannot collide.
    pub name: String,
    /// Unary or stream. See [`OpKind`].
    pub kind: OpKind,
    /// What the op does, and anything a caller has to know that the shapes do
    /// not say.
    pub description: Prose,
    /// The arguments. Either absent, or a reference to a struct: peerline
    /// rejects params that are not a JSON object, so a scalar here would be a
    /// contract nobody can call.
    #[serde(default)]
    pub args: Option<Schema>,
    /// What a [`OpKind::Unary`] op answers. Absent means an empty result.
    /// Must be absent on a stream.
    #[serde(default)]
    pub result: Option<Schema>,
    /// What a [`OpKind::Stream`] op emits, one at a time. Required on a
    /// stream, and must be absent on a unary op.
    #[serde(default)]
    pub item: Option<Schema>,
    /// The names of the errors this op can raise, from the contract's
    /// [`Manifest::errors`] table.
    #[serde(default)]
    pub errors: Vec<String>,
}

impl OpDef {
    /// The op name with the contract prefix removed — `"thread.start"` for
    /// `"codex.server.thread.start"`. The part a generator turns into an
    /// identifier.
    pub fn short_name<'a>(&'a self, contract: &str) -> &'a str {
        self.name
            .strip_prefix(contract)
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or(&self.name)
    }
}
