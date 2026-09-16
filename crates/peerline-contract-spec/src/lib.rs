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
//! The same bias shows up in smaller places, and each one is a key in this
//! format. A manifest that could only spell a wire value that happens to be a
//! legal snake identifier would be imposing one language's identifier rules on
//! the wire; see [`EnumVariant::wire`]. A manifest that could not say a field
//! defaults to `true` would leave a Python implementor unable to learn it from
//! the file that is supposed to be the contract; see [`Field::default`].
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
//!
//! # A manifest names a *contract*, never a language's path
//!
//! Two manifests may share a shape — [`Manifest::imports`] is how one says so,
//! and [`Role::Vocabulary`] is what the file being imported from is. The
//! import names a contract and a version and the type names it uses, and
//! nothing else: which Rust module, Python package or TypeScript file supplies
//! those types is a fact about *that* target, and belongs wherever that
//! target's generator is invoked. Rust's answer is the `imports` clause of
//! `peerline_contract::contract!`; a Python generator would carry its own
//! mapping, read from its own configuration, and the manifest would not
//! change.
//!
//! That rule is not specific to imports. Any key that only makes sense to one
//! language belongs at that language's generator, not in the shared file.
//!
//! # The field states, and why there are more than two
//!
//! A field is required, **absent** when it has nothing to say
//! ([`Field::optional`]), or **null** when it has nothing to say
//! ([`Field::nullable`]). The first two were the whole vocabulary once, and
//! absent-over-null remains the recommendation for a new contract: a key that
//! is not there is the one form every target agrees on, and a decoder that
//! never sees `null` needs no rule for it.
//!
//! [`Field::nullable`] exists because a manifest describes wire forms that
//! already exist, and some of them emit `null`. A format that could only
//! describe the recommended form would be a format those contracts cannot be
//! written in, which would leave them undescribed — the opposite of the point.
//! So both are expressible, the difference is stated rather than lost, and the
//! recommendation is a recommendation rather than a silent limit.
//!
//! [`Field::default`] is the fourth: absence with a **value**. And
//! [`Field::default`] *with* [`Field::optional`] is the fifth, which is the
//! same fact holding in both directions — a writer omits the key when the
//! value is the uninteresting one, and a reader that meets an absent key
//! applies it. That pair is what serde spells
//! `#[serde(default = …, skip_serializing_if = …)]`, it is the commonest
//! shape in the wires this format was written to describe, and until it was
//! expressible a contract with such a field declared one half and put the
//! other in prose. See [`Field::default`] for the case that made it worth
//! adding and for the one requirement it puts on a field's shape.
//!
//! # Composites: lists and maps
//!
//! [`Schema::Array`] and [`Schema::Map`] are the two ways a value is many
//! values. A map's keys are strings, always, because a JSON object has no
//! other kind of member name — there is no key-type keyword and nothing one
//! could say with it. Before the map form existed, a contract whose field was
//! an object of strings to strings had to declare it as passthrough `json`,
//! which says the value is not interpreted when it very much is;
//! `peerline-cluster` carries exactly that field, with a test of its own
//! pinning the gap.
//!
//! # Error codes, and the range a contract claims
//!
//! Two contracts in one directory may not pick the same code, and never
//! could. [`Manifest::codes`] is how a contract says which codes are *its* —
//! a claim on a range rather than on the numbers it happens to have spent —
//! so that the next contract's author finds out at the moment they choose,
//! rather than when a consumer mounts both. See [`CodeBand`].
//!
//! # Two integer widths are a portability fact, not a Rust one
//!
//! [`Scalar::I53`] and [`Scalar::U53`] are integers every target represents
//! exactly, bounded at ±(2^53 − 1). [`Scalar::I64`] and [`Scalar::U64`] are
//! the ones that may exceed that, and saying `i64` is therefore saying a
//! target whose only number is a double must carry the value as an
//! arbitrary-precision type or lose it.
//!
//! Both pairs exist because the distinction is only worth anything if a
//! contract can decline it. A wall-clock millisecond count, a duration, a
//! byte count and a tally all sit many orders of magnitude inside the safe
//! bound; an opaque 64-bit id does not. Before this keyword existed, the
//! second fact could only be written on the Rust side of a generator, as a
//! per-field annotation in a language no other implementor reads — which is
//! the same bias the manifest exists to remove, one layer down.
//!
//! Nothing about the wire changes: JSON has one number type, and a value
//! declared `i53` is the same bytes as one declared `i64`. What changes is
//! that a generator can *refuse*, or annotate, on the strength of what the
//! file says rather than what somebody remembered. A [`Field::default`]
//! outside the safe range is rejected here.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod directory;
mod names;
mod validate;

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

pub use directory::Directory;
pub use names::{const_name, pascal_case, screaming_case};
pub use validate::Error;

/// Arbitrary JSON, as [`Field::default`] carries it.
///
/// Re-exported so that a generator can name the type of a default without
/// having to agree separately with this crate on which `serde_json` it means.
pub use serde_json::Value;

/// One contract, described completely.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The contract name, e.g. `"codex.server"`. The same string a producer
    /// registers as `ContractId::name`, and the required prefix of every op
    /// name in this file.
    pub contract: String,
    /// The major version. One integer, additive within it — a breaking change
    /// is a new number and a new file, never an edit in place.
    pub version: u32,
    /// Whether this file describes a contract or a shared vocabulary. Absent
    /// means a contract, which is what every file written before the
    /// distinction existed is.
    #[serde(default)]
    pub role: Role,
    /// Why this contract exists and what it deliberately is not. The prose a
    /// generator puts at the top of whatever it emits.
    pub description: Prose,
    /// The other manifests whose types this one uses. See [`Import`].
    #[serde(default)]
    pub imports: Vec<Import>,
    /// The named shapes this contract's ops exchange. Every `args`, `result`,
    /// `item` and error `data` either names one of these, names an imported
    /// one, or is a scalar.
    #[serde(default)]
    pub types: Vec<TypeDef>,
    /// The range this contract's error codes are drawn from, when it claims
    /// one. See [`CodeBand`].
    ///
    /// Absent is exactly today's behaviour: the codes are checked for
    /// uniqueness and nothing else. A manifest that never declares a band is
    /// unaffected by every rule the key adds, which is what lets it be added
    /// to a directory one file at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codes: Option<CodeBand>,
    /// The error codes a producer of this contract may raise, with what each
    /// one means and what it carries. Declared once and referenced by name
    /// from the ops that can raise them, because one code is normally raised
    /// by several ops and two copies of its meaning would disagree.
    #[serde(default)]
    pub errors: Vec<ErrorDef>,
    /// The ops, in whatever order reads best — normally lifecycle order.
    /// Empty exactly when this file is a [`Role::Vocabulary`].
    #[serde(default)]
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

    /// The named types declared *here*, keyed by name. Built after
    /// validation, so every local reference in the file resolves in here.
    /// Imported names do not: see [`Manifest::imported_types`].
    pub fn type_map(&self) -> BTreeMap<&str, &TypeDef> {
        self.types.iter().map(|t| (t.name(), t)).collect()
    }

    /// Every imported type name, keyed to the import that brought it in.
    ///
    /// A generator needs both halves: the name, because that is what the
    /// schemas in this file say, and the import, because that is what its own
    /// target-specific mapping is keyed by.
    pub fn imported_types(&self) -> BTreeMap<&str, &Import> {
        self.imports
            .iter()
            .flat_map(|import| import.types.iter().map(move |ty| (ty.as_str(), import)))
            .collect()
    }

    /// The declared errors, keyed by name.
    pub fn error_map(&self) -> BTreeMap<&str, &ErrorDef> {
        self.errors.iter().map(|e| (e.name.as_str(), e)).collect()
    }

    /// The ops, keyed by the full name they travel under.
    ///
    /// The companion to [`Manifest::type_map`] and [`Manifest::error_map`],
    /// and there for the same reason: a checker comparing this file against an
    /// implementation walks the implementation's op table and asks what the
    /// manifest says about each name. Doing that against [`Manifest::ops`]
    /// directly is a linear scan per op written slightly differently by every
    /// caller.
    pub fn op_map(&self) -> BTreeMap<&str, &OpDef> {
        self.ops.iter().map(|op| (op.name.as_str(), op)).collect()
    }

    /// The id a manifest is imported by: `"coagent.rpc/1"`.
    pub fn id(&self) -> String {
        format!("{}/{}", self.contract, self.version)
    }
}

/// The range of error codes one contract claims.
///
/// # Why a contract declares a range rather than picking numbers
///
/// [`Directory::new`](crate::Directory::new) has always refused two contracts
/// that pick the same code, because a consumer matches on the code and then
/// decodes the payload behind it, so a collision is a mis-decode rather than a
/// confusing message. What it could not refuse is the *next* collision: two
/// contracts that do not collide today and will, because neither said which
/// numbers were its to spend.
///
/// That is not hypothetical, and it is not solved by care. The `coagent`
/// contracts directory partitions its four codes by hundreds — 3001, 3101,
/// 3201, 3301 — and the partition exists only as four hand-chosen literals
/// whose descriptions each say the code is "distinct from every other code in
/// this system". Every one of those sentences is true and none of them is
/// checkable: the fifth contract's author reads four descriptions asserting
/// distinctness and has nothing that says where the unclaimed numbers start.
/// `peerline-cluster`'s contracts README has the same note pointing the other
/// way — "if an error code is ever added here it must not collide with
/// `coord/1`'s 1001–1004. No checker can see that."
///
/// A band is that convention, written where a checker can read it. Declaring
/// one says two things at once: every code in this file is inside it, and no
/// other file in this directory may claim a number in it. The second half is
/// what makes a band worth more than a comment — it fails at the moment a new
/// contract's author picks a range, which is the moment they can still pick a
/// different one, rather than at the moment a consumer mounts both.
///
/// # Both ends are inclusive
///
/// `{ "start": 3100, "end": 3199 }` is a hundred codes, 3100 through 3199. An
/// exclusive end would make the natural spelling of a hundred-wide band
/// `3100..3200`, whose last digit belongs to the *next* band — which is the
/// off-by-one that the partition exists to prevent, reintroduced in the key
/// that states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeBand {
    /// The lowest code this contract may use. Inclusive.
    pub start: i32,
    /// The highest code this contract may use. Inclusive, and not less than
    /// [`Self::start`] — a band that holds nothing is a claim on nothing.
    pub end: i32,
}

impl CodeBand {
    /// Whether a code falls inside this band.
    pub fn holds(&self, code: i32) -> bool {
        (self.start..=self.end).contains(&code)
    }

    /// Whether two bands share any code at all.
    ///
    /// The whole of what makes two declared bands incompatible: a directory
    /// may hold any number of bands, and they may sit anywhere, as long as no
    /// number is claimed twice.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

impl fmt::Display for CodeBand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..={}", self.start, self.end)
    }
}

/// What a manifest file is.
///
/// Two roles, because there turned out to be two things a manifest is written
/// for, and only one of them has ops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A contract: ops a producer registers and a consumer calls. The
    /// default, and what a file that says nothing is.
    #[default]
    Contract,
    /// A shared vocabulary: named shapes and nothing else, for other
    /// manifests to [`Manifest::imports`].
    ///
    /// Nobody produces a vocabulary and nobody calls it. It exists because a
    /// shape can travel in several contracts — a session's identity does —
    /// and three restatements of it would be three things to keep in step
    /// with nothing saying they are the same shape. That is worse for exactly
    /// the reader this format serves: an implementor who cannot fall back to
    /// somebody's code.
    ///
    /// A vocabulary therefore declares no ops, no errors and no
    /// [`Manifest::codes`] band. An error code is something a *producer*
    /// raises, and a vocabulary has no producer; a band reserving codes for a
    /// producer that does not exist would take numbers out of the directory's
    /// one code space on behalf of nobody.
    Vocabulary,
}

/// Types taken from another manifest.
///
/// Named by contract and version, exactly as a consumer names a contract it
/// dials — not by file path and not by any language's module path, because
/// the file layout is a property of a checkout and the module path is a
/// property of one target. What a Rust crate, a Python package or a
/// TypeScript module calls the imported shapes is supplied where that
/// generator is invoked.
///
/// Every language-neutral IDL has this and for the same reason: protobuf's
/// `import`, JSON Schema's `$ref`. The alternative — each manifest restating
/// the shape — hands an implementor several definitions with nothing
/// asserting they are the same one, and nothing to catch them drifting apart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    /// The contract or vocabulary name, e.g. `"coagent.rpc"`.
    pub contract: String,
    /// Its major version. Part of the identity: `coagent.rpc/1` and
    /// `coagent.rpc/2` are different files describing different shapes.
    pub version: u32,
    /// The type names used from it. Listed rather than glob-imported so that
    /// the set of shapes this contract's wire depends on is readable here,
    /// and so a name that disappears from the far side is a failure with
    /// something to point at.
    pub types: Vec<String>,
    /// Why this contract shares those shapes rather than declaring its own.
    /// Required, like every other description: an import is a coupling
    /// between two contracts and the reason for it is contract documentation.
    pub description: Prose,
}

impl Import {
    /// The id this import names: `"coagent.rpc/1"`.
    pub fn id(&self) -> String {
        format!("{}/{}", self.contract, self.version)
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

/// A type reference: a scalar keyword, a named type, an array or map of
/// either, or one of those or null.
///
/// Written as a bare string in the common case (`"string"`, `"Fault"`), as
/// `{ "array": … }` for a list, as `{ "map": … }` for an object of string
/// keys, and as `{ "nullable": … }` where the value may be JSON `null`. A
/// bare string that is not one of the eleven scalar keywords is a reference to
/// a [`TypeDef`] by name or to an imported type, and the validator rejects one
/// that does not resolve — so a misspelled scalar fails loudly rather than
/// becoming a dangling reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Schema {
    /// A scalar keyword, the name of a [`TypeDef`], or an imported name.
    Named(String),
    /// A list of the inner schema.
    Array {
        /// What the list holds.
        array: Box<Schema>,
    },
    /// A JSON object whose keys are arbitrary strings and whose values are
    /// all the inner schema.
    ///
    /// # There is no key-type keyword, and there is nothing one could say
    ///
    /// A JSON object's keys are strings. Not "strings by convention" —
    /// strings by grammar: there is no other kind of member name in the
    /// format this contract travels in, and a target that decodes a JSON
    /// object into a dictionary keyed by anything else has done a conversion
    /// the wire did not describe. So a `key` keyword beside this one would
    /// have exactly one legal value, which is a knob that documents nothing
    /// and gives every generator a case to handle that no manifest can
    /// reach. The keys are strings; that is said here, once, instead.
    ///
    /// What a contract *can* still say about the keys is what they mean —
    /// environment variable names, session ids, locale tags — and that is
    /// prose, on the field, like every other thing the shape does not carry.
    ///
    /// Position rules are an array's: legal wherever a list is, which is to
    /// say anywhere except as an op's `args` (peerline wants a struct there,
    /// not an object of unknown keys) and except as an internally tagged
    /// arm's payload (there is nowhere inside an object of unknown keys to
    /// put a discriminator without possibly colliding with one). A map-typed
    /// field may not be [`Field::flatten`]ed; see that key for why.
    Map {
        /// What each value is. The keys are strings and are not declared.
        map: Box<Schema>,
    },
    /// The inner schema, or JSON `null`.
    ///
    /// Only legal where there is no field to carry [`Field::nullable`] — an
    /// op's `result` or `item`, or an error's `data`. A field says it the
    /// other way, because a field also has to distinguish *absent* from
    /// *null* and a schema cannot. One spelling per idea; the validator
    /// rejects the other one where it would be a second.
    Nullable {
        /// What the value is when it is not null.
        nullable: Box<Schema>,
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
    /// A signed integer every target represents exactly, bounded at
    /// ±(2^53 − 1).
    ///
    /// The digit is the *magnitude* width, not a two's-complement field
    /// width: `i53` is the symmetric range −9007199254740991 ..=
    /// 9007199254740991, which is what a target whose only number is a
    /// double actually guarantees. That is the whole content of the
    /// keyword — a promise about the value, made in the contract, where
    /// every implementor reads it.
    ///
    /// Say this, not [`Self::I64`], for a millisecond timestamp, a
    /// duration, a byte count or a tally. All of them sit many orders of
    /// magnitude inside the bound, and saying `i64` instead asks every
    /// target with a 53-bit exact range to either lose precision silently
    /// or promote the value to something the JSON cannot be compared
    /// against. See the crate docs on the safe-integer scalars.
    I53,
    /// An unsigned integer every target represents exactly, bounded at
    /// 2^53 − 1. The unsigned half of [`Self::I53`]; same reasoning.
    U53,
    /// A signed 64-bit integer, **whose value may exceed what a target
    /// with a 53-bit exact range can hold**.
    ///
    /// This is a warning, not a default. A contract that says `i64` is
    /// saying it genuinely needs the range past ±(2^53 − 1) — and is
    /// therefore saying that a JavaScript implementor must carry the value
    /// as a `bigint`, or lose it. peerline's own `vectors.json` marks two
    /// entries `tsOnly` for exactly this reason: an id past 2^53 is a legal
    /// `u64` and is not a JavaScript number.
    ///
    /// If the value is a timestamp, a duration, a count or a size, it does
    /// not need the range and [`Self::I53`] is the honest declaration.
    I64,
    /// An unsigned 64-bit integer. Same warning as [`Self::I64`], with the
    /// bound at 2^64 − 1; [`Self::U53`] is the one to reach for unless the
    /// range is genuinely needed.
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
            Self::I53 => "i53",
            Self::U53 => "u53",
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
            "i53" => Self::I53,
            "u53" => Self::U53,
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

    /// Whether a value of this scalar is a fixed-size thing a target may pass
    /// by value. Text, paths and passthrough JSON are not; the numbers and
    /// the boolean are.
    ///
    /// Language-neutral in the same way [`Self::is_exact`] is: it is a
    /// property of the declared shape, read off the manifest rather than
    /// stated by it, and a target with no such notion ignores it.
    pub fn is_small(self) -> bool {
        matches!(
            self,
            Self::Bool
                | Self::I32
                | Self::U32
                | Self::I53
                | Self::U53
                | Self::I64
                | Self::U64
                | Self::F64
        )
    }
}

/// The largest magnitude a [`Scalar::I53`] or [`Scalar::U53`] may carry:
/// 2^53 − 1, the last integer a double represents exactly.
///
/// Public because a generator validating its own literals wants the same
/// number this crate validates defaults against, and two copies of it would
/// be two chances to write one of them wrong.
pub const SAFE_INTEGER_MAX: i64 = 9_007_199_254_740_991;

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.keyword())
    }
}

/// One named shape.
///
/// Four forms, because four is what the wire actually uses: a record, a
/// closed set of names, a tagged union of records, and a **name for a shape
/// that is already something else**. Anything else is expressible as a
/// composition of these, and adding a fifth is a change to every generator at
/// once.
///
/// The fourth is the odd one and is worth saying why it is here. A
/// [`Self::Newtype`] adds nothing to the wire — it serialises as whatever it
/// wraps, byte for byte — so on the face of it it is a form that says
/// nothing. What it says is that two values of the same *representation* are
/// not the same *thing*: a host id and a session id are both strings, and a
/// contract that calls them both `string` has described a wire nobody can
/// mix up and an API everybody can. Which targets can act on that varies —
/// Rust makes it a distinct type for free, TypeScript cannot without
/// breaking assignability from parsed JSON — and each generator's answer is
/// its own, read off the same declaration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// A tagged union. See [`Tagging`] for the two forms the tag takes.
    Union {
        /// The type's name.
        name: String,
        /// What the union is for.
        description: Prose,
        /// How the arm is identified on the wire. Absent means
        /// [`Tagging::Internal`], which is what every union written before
        /// the key existed is.
        #[serde(default)]
        tagging: Tagging,
        /// The field carrying the discriminator, e.g. `"kind"`. Required for
        /// [`Tagging::Internal`] and forbidden for [`Tagging::External`],
        /// which has no discriminator field — the key *is* the arm.
        #[serde(default)]
        tag: Option<String>,
        /// The variants, each with its own payload.
        variants: Vec<UnionVariant>,
    },
    /// A name for a shape that is already something else, carrying no keys of
    /// its own and no discriminator.
    ///
    /// ```jsonc
    /// { "form": "newtype", "name": "HostId", "newtype": "string",
    ///   "description": ["Which host, in the coordinator's namespace."] }
    /// ```
    ///
    /// **Wire-identical to what it wraps.** A `HostId` on the wire is the
    /// string; there is no envelope, no tag and no extra object. That is the
    /// whole constraint on a generator: whatever it emits must serialise and
    /// deserialise as the inner schema does, or the name has changed the
    /// contract rather than described it.
    ///
    /// What it is *for* is the distinction between representation and
    /// meaning. `peerline-cluster` records the motivating case: its `HostId`
    /// is a string, its connection dialling and its socket-reachability check
    /// are both keyed on the type, and a manifest that could only say
    /// `string` would describe a contract whose Rust cannot be written the way
    /// the hand-written Rust already is.
    ///
    /// The inner schema follows a struct field's position rules: a scalar, a
    /// named shape, a list, a map, or a composition of those, but not a
    /// nullable *schema* — "or null" about a named thing is
    /// `{ "nullable": "HostId" }` at the place it is used, where the reader
    /// can see it, rather than baked into the name so that every use of it is
    /// silently optional.
    ///
    /// A newtype may not wrap itself, directly or through a chain of other
    /// newtypes: the value would have no size in any target that generates
    /// records, for the same reason a struct containing itself does not.
    Newtype {
        /// The type's name.
        name: String,
        /// What this name is for, and what it stops being confused with.
        description: Prose,
        /// The shape this is a name for.
        ///
        /// Spelled `newtype`, repeating the `form`, rather than `type` as a
        /// field's is. A type definition's payload key is already its form's
        /// own word — `fields`, `variants` — and reading `"newtype": "string"`
        /// beside `"form": "newtype"` is how every other definition in the
        /// file reads. `type` would also be the one key in this format that
        /// means two different things depending on which object it is in.
        newtype: Schema,
    },
}

impl TypeDef {
    /// The declared name.
    pub fn name(&self) -> &str {
        match self {
            Self::Struct { name, .. }
            | Self::Enum { name, .. }
            | Self::Union { name, .. }
            | Self::Newtype { name, .. } => name,
        }
    }

    /// The prose describing it.
    pub fn description(&self) -> &Prose {
        match self {
            Self::Struct { description, .. }
            | Self::Enum { description, .. }
            | Self::Union { description, .. }
            | Self::Newtype { description, .. } => description,
        }
    }
}

/// How a union says which arm it is.
///
/// Both forms are on wires that exist, which is the only reason there are
/// two. A format that could describe one of them would be a format the other
/// contract cannot be written in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tagging {
    /// The discriminator is a key *inside* the object, beside the arm's own
    /// keys: `{"kind": "line", "text": "…"}`. The default, and the form to
    /// prefer for a new contract — a reader can dispatch on one key without
    /// knowing the arm names in advance.
    #[default]
    Internal,
    /// The object has exactly one key, which is the arm name, and its value
    /// is the arm's payload: `{"only": ["turn_started"]}`.
    ///
    /// It carries a payload that is not an object, which internal tagging
    /// cannot: there is nowhere to put a discriminator key inside a JSON
    /// array or a bare string.
    External,
}

/// One field of a struct or of a union variant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// The wire name, in the manifest's own spelling. This is the JSON key —
    /// except under [`Self::flatten`], where there is no key and the name is
    /// only what a generator calls the member.
    pub name: String,
    /// What the field holds.
    #[serde(rename = "type")]
    pub ty: Schema,
    /// Whether the field may be **absent**. An absent optional field is not
    /// the same as a present null one in every target, so a generator omits
    /// it on the way out rather than writing `null`. The recommended way to
    /// say "nothing to say": see the crate docs.
    ///
    /// On its own, absence means *nothing was said*. Together with
    /// [`Self::default`] it means something more specific — absence is legal
    /// in both directions *and* carries a value — which is a fourth state and
    /// is described on that key.
    #[serde(default)]
    pub optional: bool,
    /// Whether the field may be **null**: the key is always written, and its
    /// value is `null` when there is nothing to say.
    ///
    /// Mutually exclusive with [`Self::optional`] — a field says one or the
    /// other, and which one is a fact about the wire that a decoder in
    /// another language needs. A nullable field still *accepts* an absent key
    /// on the way in, because a decoder that tolerates less than it emits
    /// helps nobody; what it does not do is emit one.
    ///
    /// Also mutually exclusive with [`Self::default`], and for a reason that
    /// does not weaken now that `optional` and `default` combine: a default
    /// is what an **absent** key means, and a nullable field's key is never
    /// absent on the way out. `null` and the default would be two answers to
    /// one question, with nothing saying which a reader that saw an explicit
    /// `null` should take.
    #[serde(default)]
    pub nullable: bool,
    /// The value a reader assumes when the key is absent.
    ///
    /// Not the same as [`Self::optional`]. An optional field's absence means
    /// *nothing was said*, and every reader sees that same nothing; a default
    /// means absence has a **value**, and a reader that does not know the
    /// value gets a different answer than one that does. That is why it is in
    /// the manifest rather than in each implementation: a Python implementor
    /// reading a field with no default cannot learn that an absent
    /// `include_history` means `true`, and guessing `false` would silently
    /// turn a listing into something else.
    ///
    /// Written as JSON of the field's own declared type, and validated
    /// against it here so that a default a generator cannot emit is a parse
    /// failure rather than a surprise in one target.
    ///
    /// # With [`Self::optional`]: the pair the format could not spell
    ///
    /// On its own, a default says absence is legal **on the way in** and
    /// carries a value; a writer still writes the key every time. Saying
    /// `optional` as well says absence is legal in **both** directions: a
    /// writer omits the key when the value is the declared default, and a
    /// reader that meets an absent key applies it. The two halves are one
    /// statement — *this value is the uninteresting one, and the uninteresting
    /// one is not written down* — and it is the single most common shape serde
    /// produces, `#[serde(default = …, skip_serializing_if = …)]`.
    ///
    /// Before the pair was expressible, a contract with such a field had to
    /// declare one half and put the other in prose. `peerline-cluster`'s
    /// `SpawnDiagnostic` is the recorded case: all six of its fields are
    /// `optional`, because that is the statement about the wire, with the
    /// meaning of an absent key written out in each field's description —
    /// "absent means none", for a counter that is zero almost always "and is
    /// why it is omitted rather than written out". That sentence *is* this
    /// pair, said in a place no generator reads.
    ///
    /// The cost is one requirement the other states do not have: a writer
    /// deciding whether to omit the key has to compare the value against the
    /// default, so the field's shape must be one this format says can be
    /// compared exactly. A shape holding an `f64` or a passthrough `json`
    /// cannot — the format says so itself, in [`Scalar::is_exact`] — and such
    /// a field is rejected rather than given a predicate whose answer depends
    /// on a comparison the contract disclaims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Whether this field's own keys are written **beside** its siblings
    /// rather than under its name.
    ///
    /// The wire has no key for a flattened field; its type's keys appear
    /// directly in the enclosing object. This exists because a contract that
    /// forwards another's arguments does exactly that — the forwarded shape's
    /// keys are the ones on the wire, and re-spelling them here would be a
    /// second definition to keep in step.
    ///
    /// The name is still required, because a generator needs something to
    /// call the member. It is the one place in this format where `name` is
    /// not a JSON key, and the prose on such a field should say what it
    /// carries.
    ///
    /// # Not a map
    ///
    /// The field's type must be a named object shape. A **map** is refused
    /// here specifically, and not as an oversight: flattening a map is a
    /// thing serde will happily do, and what it means is *every key the
    /// enclosing object has left over lands in here* — a catch-all for
    /// unknown keys. That is a real and useful wire form and it is not one
    /// any manifest in this format declares: every shape here is closed, a
    /// key nobody declared is a decoding failure, and a flattened map would
    /// quietly turn the enclosing object into one that accepts anything. If a
    /// contract ever genuinely wants a catch-all it is a thing the format
    /// should learn to *say*, with the openness visible at the shape, rather
    /// than something a reader has to infer from the type of a flattened
    /// field.
    #[serde(default)]
    pub flatten: bool,
    /// What this field means and why it is here. Required — see the crate
    /// docs.
    pub description: Prose,
}

impl Field {
    /// Whether a caller must supply this field: it is not absent-able, not
    /// null-able and has no default.
    ///
    /// What a generated constructor takes, and what a checker in another
    /// language demands of an incoming object.
    pub fn is_required(&self) -> bool {
        !self.optional && !self.nullable && self.default.is_none()
    }

    /// Which of the states this field is in, as one value.
    ///
    /// The states are spelled on [`Field`] as independent keys because that is
    /// what reads well in the file, and the validator has already rejected the
    /// combinations that are not a state — optional and nullable together, or
    /// nullable with a default. A reader that has a validated manifest in hand
    /// is therefore looking at exactly one of [`FieldState`]'s arms, and
    /// matching on it is how a checker gets told by its own compiler when a
    /// state is added to the format. Booleans tested in the wrong order would
    /// just quietly pick one.
    pub fn state(&self) -> FieldState<'_> {
        match (&self.default, self.nullable, self.optional) {
            (Some(default), _, true) => FieldState::OptionalDefault(default),
            (Some(default), _, false) => FieldState::Default(default),
            (None, true, _) => FieldState::Nullable,
            (None, false, true) => FieldState::Optional,
            (None, false, false) => FieldState::Required,
        }
    }
}

/// What a field's absence from an object means.
///
/// See [`Field::state`], and the crate docs on the field states for why
/// absent and null are not one answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FieldState<'a> {
    /// The key is always written, and an object without it is malformed.
    Required,
    /// The key may be absent, and absence means nothing was said. A writer
    /// omits it rather than writing `null`. See [`Field::optional`].
    Optional,
    /// The key is always written, and its value is `null` when there is
    /// nothing to say. See [`Field::nullable`].
    Nullable,
    /// Absence has a **value**, carried here: a reader that does not apply it
    /// gets a different answer than one that does. The key is nevertheless
    /// always written on the way out, so a reader never has to apply it to
    /// something this contract's own producer wrote. See [`Field::default`].
    Default(&'a Value),
    /// Absence has a **value**, carried here, **and is legal on the way out**:
    /// a writer omits the key exactly when the value equals this one.
    ///
    /// The difference from [`Self::Default`] is a direction, not a value. Both
    /// say what an absent key means; this one also says that a writer of this
    /// contract will produce absent keys, so a decoder has to apply the
    /// default to real traffic rather than only to hand-written requests. See
    /// [`Field::default`] for the pair this is, and for the one shape
    /// requirement it carries that no other state does.
    OptionalDefault(&'a Value),
}

impl FieldState<'_> {
    /// The value an absent key means, when it means one.
    ///
    /// The two defaulted states differ in what a *writer* does, and a reader
    /// applying a default does not care which: asking for the value through
    /// this is how a decoder avoids matching two arms to do one thing, and
    /// how it stops compiling rather than silently ignoring a third if the
    /// format ever grows one.
    pub fn default_value(&self) -> Option<&Value> {
        match self {
            Self::Default(value) | Self::OptionalDefault(value) => Some(value),
            Self::Required | Self::Optional | Self::Nullable => None,
        }
    }

    /// Whether a writer may leave this key out.
    ///
    /// True for [`Self::Optional`] and [`Self::OptionalDefault`], which are
    /// the two states whose whole content is about the way out. A
    /// [`Self::Nullable`] key is written as `null` and a [`Self::Default`] one
    /// is written as its value, so neither is absent on a wire this contract
    /// produced — even though both tolerate an absent key coming in.
    pub fn may_be_absent(&self) -> bool {
        matches!(self, Self::Optional | Self::OptionalDefault(_))
    }
}

/// One value of a string enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumVariant {
    /// The variant's name in the manifest's own spelling, `lower_snake_case`.
    ///
    /// This is the **identifier**, not necessarily the wire value: every
    /// generator derives its own spelling from it — `GoneAway`, `GONE_AWAY`,
    /// `goneAway` — and they have to derive it from the same string or two
    /// bindings document different contracts.
    pub name: String,
    /// The literal JSON text of this value, when it is not [`Self::name`].
    ///
    /// A wire value is a string, and a string is not an identifier. Requiring
    /// it to be a legal `lower_snake_case` word would be imposing one
    /// language's identifier rules on the wire — in a file whose entire
    /// premise is that it is not one language's — and it would make contracts
    /// whose values are `"claude-code"` or `"turn-started"` inexpressible,
    /// including ones with live non-Rust consumers reading exactly those
    /// spellings.
    ///
    /// So `name` stays the cross-generator identifier and `wire` carries the
    /// text. Absent means they are the same, which every manifest written
    /// before this key assumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<String>,
    /// What it means, and what a reader should do differently on seeing it.
    pub description: Prose,
}

impl EnumVariant {
    /// The literal JSON text for this value: [`Self::wire`] if it was given,
    /// [`Self::name`] otherwise.
    pub fn wire(&self) -> &str {
        self.wire.as_deref().unwrap_or(&self.name)
    }
}

/// One arm of a tagged union.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnionVariant {
    /// The arm's name in the manifest's own spelling, `lower_snake_case`.
    /// The identifier, exactly as [`EnumVariant::name`] is.
    pub name: String,
    /// The literal JSON text of the discriminator for this arm, when it is
    /// not [`Self::name`]. Same reasoning as [`EnumVariant::wire`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<String>,
    /// What this arm means.
    pub description: Prose,
    /// The fields this arm carries. Mutually exclusive with
    /// [`Self::payload`].
    #[serde(default)]
    pub fields: Vec<Field>,
    /// A named shape this arm carries *whole*, instead of restating its
    /// fields.
    ///
    /// Under [`Tagging::Internal`] the payload must be an object type, and
    /// its keys sit beside the discriminator — the same wire form as spelling
    /// the fields out here. The difference is that the shape stays one shape:
    /// a consumer that already holds it can put it on the stream and take it
    /// off again, rather than destructuring it at one end and rebuilding it
    /// at the other.
    ///
    /// Under [`Tagging::External`] the payload may be anything, including a
    /// list — which is the case internal tagging cannot express at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Schema>,
}

impl UnionVariant {
    /// The literal JSON text of this arm's discriminator.
    pub fn wire(&self) -> &str {
        self.wire.as_deref().unwrap_or(&self.name)
    }
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
    ///
    /// A result that is *sometimes* there is a nullable schema —
    /// `{ "nullable": "SessionInfo" }` — and not an absent one: absent here
    /// means the op answers nothing at all, every time, which is a different
    /// contract.
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
