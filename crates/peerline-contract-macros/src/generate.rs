//! Turning a validated manifest into Rust.
//!
//! Nothing here decides anything a manifest left open: the manifest is
//! validated before this module sees it, so every reference resolves, every
//! description exists, and every op's kind agrees with its shape. What is
//! left is one mapping — manifest spelling to Rust spelling — and it is the
//! same mapping a checker in another language would make into its own.
//!
//! The one exception is the thing a manifest deliberately does not say,
//! supplied at the call site and about *this* target rather than about the
//! contract: which Rust module supplies an imported shape. See [`Bindings`].
//!
//! Everything else an imported shape needs is read out of the manifest that
//! declares it, which the [`Directory`] has open. That is the whole of what
//! loading a directory rather than a file buys: an imported shape is no
//! longer a name the expansion has to be told about.
//!
//! # A generated set is closed
//!
//! No generated enum or union carries `#[non_exhaustive]`, and that is a
//! decision rather than an omission. The attribute buys a reader
//! source-level tolerance for a variant it has not heard of — but the
//! expansion emits no catch-all variant, so serde **rejects** an unknown
//! value at the decode boundary. A Rust program can therefore never *hold* a
//! value outside its compiled set, unless it was built against the newer
//! manifest, in which case it has the variant. The attribute purchases
//! nothing, and pays for it by turning "a manifest edit changes the Rust or
//! fails to compile" into a silent wildcard for every consumer outside the
//! crate that expanded the manifest. The compile error is the property; it
//! stays.
//!
//! Where openness would have to be said, if it were ever wanted, is the
//! manifest — not a Rust attribute name in a language-neutral file, and not
//! a call-site clause, because openness is a property of the contract and not
//! of this build. The format has one answer for closedness today and
//! the wire agrees with it. If it ever gains a way to say the other, the
//! generator emits the catch-all variant **and** the attribute together, so
//! that both halves of the generated code say the same thing.

use std::collections::{BTreeMap, BTreeSet};

use peerline_contract_spec::{
    Directory, ErrorDef, Field, Manifest, OpDef, OpKind, Prose, Role, Scalar, Schema, Tagging,
    TypeDef, UnionVariant, Value, pascal_case, screaming_case,
};
use proc_macro2::{Literal, Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Ident, LitStr};

/// How far up the comparison ladder a shape goes.
///
/// One ordered scale rather than a set of trait names, because the rungs are
/// nested: a shape that can be copied can be compared for total equality, and
/// one that can be compared totally can be compared partially. A struct's
/// rung is the lowest of its fields', which is the whole computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Support {
    /// `PartialEq`. What a shape holding an `f64` or passthrough JSON
    /// reaches.
    Partial,
    /// `PartialEq + Eq`. What most wire shapes are.
    Total,
    /// `PartialEq + Eq + Copy`. Fixed-size scalars only, and never through a
    /// list.
    Copy,
}

/// What the call site says about this target, which the manifest does not.
pub(crate) struct Bindings {
    /// Imported type name to the Rust module path supplying it.
    paths: BTreeMap<String, TokenStream>,
    /// The shapes the call site asked to be exported to TypeScript. Empty
    /// when the clause is absent, which is the case for every crate that
    /// has no business linking `ts_rs`.
    typescript: BTreeSet<String>,
    /// Whether the call site asked for the runtime stubs — the producer
    /// trait, its registration, and the client.
    ///
    /// A call-site clause and not something read off the manifest, for the
    /// same reason `typescript` is one: the stubs name `::peerline` paths,
    /// and whether this crate has a peer runtime linked at all is a fact
    /// about *this* build. See [`runtime`].
    runtime: bool,
}

impl Bindings {
    /// Match the call site's clauses against the manifest's imports.
    ///
    /// Both directions are checked. An import the call site did not map
    /// cannot be generated at all; a mapping for an import the manifest does
    /// not declare is a mapping that silently does nothing, which is how a
    /// renamed vocabulary would go unnoticed until a type failed to resolve
    /// somewhere else.
    pub(crate) fn resolve(
        manifest: &Manifest,
        imports: &[(LitStr, TokenStream)],
        typescript: &[Ident],
        runtime: bool,
    ) -> syn::Result<Self> {
        let span = Span::call_site();
        let mut given: BTreeMap<String, (&LitStr, &TokenStream)> = BTreeMap::new();
        for (id, path) in imports {
            if given.insert(id.value(), (id, path)).is_some() {
                return Err(syn::Error::new(
                    id.span(),
                    format!("`{}` is mapped twice", id.value()),
                ));
            }
        }

        let declared: BTreeMap<String, &peerline_contract_spec::Import> = manifest
            .imports
            .iter()
            .map(|import| (import.id(), import))
            .collect();
        for (id, (literal, _)) in &given {
            if !declared.contains_key(id) {
                return Err(syn::Error::new(
                    literal.span(),
                    format!(
                        "`{id}` is mapped here but the manifest imports nothing of that name; \
                         the manifest is the contract, so the mapping is what is wrong"
                    ),
                ));
            }
        }

        let mut paths = BTreeMap::new();
        for (id, import) in &declared {
            let Some((_, path)) = given.get(id) else {
                return Err(syn::Error::new(
                    span,
                    format!(
                        "the manifest imports `{id}`, and nothing here says which Rust module \
                         supplies its types; add `imports {{ \"{id}\" => ::some_crate }}`"
                    ),
                ));
            };
            for name in &import.types {
                paths.insert(name.clone(), (*path).clone());
            }
        }

        let exported = resolve_typescript(manifest, &paths, typescript)?;

        // A vocabulary has no ops, so there is nothing for a producer to
        // implement and nothing for a client to call. Saying `runtime` at
        // such a call site is a mistake about which manifest is being
        // expanded, and silently generating nothing would leave it to be
        // found as a missing trait somewhere else.
        if runtime && manifest.role == Role::Vocabulary {
            return Err(syn::Error::new(
                span,
                format!(
                    "`{}` is a vocabulary: it declares shapes and no ops, so there is no producer \
                     to implement and no client to call; drop the `runtime` clause",
                    manifest.id()
                ),
            ));
        }

        Ok(Self {
            paths,
            typescript: exported,
            runtime,
        })
    }

    /// Whether this shape carries a `ts_rs::TS` derive.
    fn is_exported(&self, name: &str) -> bool {
        self.typescript.contains(name)
    }

    /// The Rust path for a named shape: a local ident, or the call site's
    /// module path plus the ident.
    fn type_path(&self, name: &str, span: Span) -> TokenStream {
        let ident = format_ident!("{}", name, span = span);
        match self.paths.get(name) {
            Some(prefix) => quote! { #prefix::#ident },
            None => quote! { #ident },
        }
    }
}

/// Match the `typescript` clause against the manifest's own shapes.
///
/// Three ways the clause can be wrong, and all three are a mistake worth a
/// message rather than a trait error inside `ts_rs`: naming a shape this
/// manifest does not declare, naming one twice, and naming one that holds a
/// shape the clause left out — the last because `ts_rs` derives recursively
/// and a member without the derive fails to compile somewhere that does not
/// mention the clause at all.
fn resolve_typescript(
    manifest: &Manifest,
    imported: &BTreeMap<String, TokenStream>,
    asked: &[Ident],
) -> syn::Result<BTreeSet<String>> {
    let local: BTreeMap<&str, &TypeDef> = manifest.type_map();
    let mut exported: BTreeSet<String> = BTreeSet::new();
    for name in asked {
        let key = name.to_string();
        if imported.contains_key(&key) {
            return Err(syn::Error::new(
                name.span(),
                format!(
                    "`{key}` is imported from another manifest, so the Rust type is the one that \
                     manifest's own expansion declares; ask for its TypeScript at that call site"
                ),
            ));
        }
        if !local.contains_key(key.as_str()) {
            return Err(syn::Error::new(
                name.span(),
                format!("`{key}` is not a shape this manifest declares"),
            ));
        }
        if !exported.insert(key.clone()) {
            return Err(syn::Error::new(
                name.span(),
                format!("`{key}` is named twice; one clause carries every shape"),
            ));
        }
    }

    for name in asked {
        let def = local[name.to_string().as_str()];
        for member in member_names(def) {
            if imported.contains_key(&member) || exported.contains(&member) {
                continue;
            }
            if !local.contains_key(member.as_str()) {
                continue;
            }
            return Err(syn::Error::new(
                name.span(),
                format!(
                    "`{name}` is exported to TypeScript and holds a `{member}`, which is not; \
                     `ts_rs` derives through a shape's members, so add `{member}` to the clause"
                ),
            ));
        }
    }
    Ok(exported)
}

/// Every named shape a definition reaches directly.
fn member_names(def: &TypeDef) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |schema: &Schema| collect_named(schema, &mut out);
    match def {
        TypeDef::Struct { fields, .. } => fields.iter().for_each(|f| push(&f.ty)),
        TypeDef::Enum { .. } => {}
        TypeDef::Union { variants, .. } => {
            for variant in variants {
                variant.fields.iter().for_each(|f| push(&f.ty));
                if let Some(payload) = &variant.payload {
                    push(payload);
                }
            }
        }
    }
    out
}

fn collect_named(schema: &Schema, out: &mut Vec<String>) {
    match schema {
        Schema::Array { array } => collect_named(array, out),
        Schema::Nullable { nullable } => collect_named(nullable, out),
        Schema::Named(name) => {
            if Scalar::from_keyword(name).is_none() {
                out.push(name.clone());
            }
        }
    }
}

/// Everything this generator knows about one [`Scalar`], in one row.
///
/// # Why there is a table, and why it is here rather than in the spec
///
/// The manifest's eleven scalar keywords used to be matched on in five
/// separate places — the Rust type, the Rust name in a doc line, the
/// TypeScript override, the Rust literal a default becomes, and the
/// validator's check of a default's JSON. Four of those are the *same
/// question* asked four times ("what does this keyword become in the targets
/// this crate emits?"), and four parallel matches is four chances for a
/// twelfth keyword to be added to three of them. So they are one table, and
/// a new scalar is one row in it.
///
/// It lives in this crate and not in `peerline-contract-spec` on purpose.
/// The spec crate's own stance is that it "does not generate anything", and
/// that anything which only makes sense to one language belongs at that
/// language's generator rather than in the shared file. `::std::path::PathBuf`
/// and `unknown` are exactly that: they would be a Rust-shaped and a
/// TypeScript-shaped hole in the crate whose whole claim is that it has
/// neither. A Python generator reads the same eleven keywords and writes its
/// own table, which is the arrangement working rather than duplication.
///
/// The fifth match — `validate::check_scalar_value` — stays in the spec, and
/// is not this question at all: "is this JSON a legal value of this keyword"
/// is a fact about the manifest, answered identically for every target, and
/// a checker in another language needs it without needing any of these
/// columns.
struct ScalarTarget {
    /// The Rust type the keyword generates, spelled absolutely because the
    /// expansion lands in a crate whose imports this macro cannot see.
    rust: &'static str,
    /// The same type as a doc line names it: what a caller writes, without
    /// the leading `::` and the module path nobody says out loud.
    doc: &'static str,
    /// The TypeScript this maps to when `ts_rs` reading the Rust alone would
    /// get it wrong, and `None` when it would not.
    ///
    /// Two rows carry one. A safe integer generates an `i64`, which `ts_rs`
    /// maps to `bigint` — correct for a value that may exceed the safe range
    /// and wrong for one the contract says cannot. Passthrough JSON generates
    /// a `serde_json::Value`, which `ts_rs` maps to a named `JsonValue`
    /// import; the contract says the value is uninterpreted, and `unknown` is
    /// how TypeScript spells that. Everything else is left alone
    /// deliberately: `i64` really is a `bigint`, and saying so is the warning
    /// the keyword exists to give.
    ts: Option<&'static str>,
    /// How a manifest default of this type becomes a Rust expression. Grouped
    /// rather than per-scalar because seven of the eleven keywords produce a
    /// bare numeric literal and differ only in which `serde_json` accessor
    /// reads them.
    literal: LiteralForm,
}

/// The shape of the Rust expression a manifest default turns into.
///
/// A small closed set rather than a second match on [`Scalar`]: the question
/// "how is this value written as Rust" has six answers across eleven
/// keywords, and naming the six is what stops the seventh being invented by
/// accident.
#[derive(Clone, Copy)]
enum LiteralForm {
    /// `true` / `false`, written as itself.
    Bool,
    /// A `String`, built from a string literal.
    Text,
    /// A `PathBuf`, built from a string literal.
    Path,
    /// A double, written as an unsuffixed float literal.
    Float,
    /// A signed integer, written as an unsuffixed integer literal.
    Signed,
    /// An unsigned integer, written as an unsuffixed integer literal.
    Unsigned,
    /// Passthrough JSON: the one form with no Rust literal at all, so the
    /// JSON text is emitted and parsed once at run time.
    Json,
}

/// The one place the manifest's spelling of a scalar becomes this target's.
fn scalar_target(scalar: Scalar) -> ScalarTarget {
    let row = |rust, doc, ts, literal| ScalarTarget {
        rust,
        doc,
        ts,
        literal,
    };
    match scalar {
        Scalar::String => row("::std::string::String", "String", None, LiteralForm::Text),
        Scalar::Bool => row("bool", "bool", None, LiteralForm::Bool),
        Scalar::I32 => row("i32", "i32", None, LiteralForm::Signed),
        Scalar::U32 => row("u32", "u32", None, LiteralForm::Unsigned),
        // A safe integer is the same Rust as the wide one: the bound is a
        // promise about the value, and Rust's narrowest exact integer that
        // holds it is still 64 bits. What the keyword buys is that a target
        // whose numbers are doubles can be told the truth — which is the
        // `ts` column — and that a generator can annotate on the strength of
        // the file instead of a remembered per-field attribute.
        Scalar::I53 => row("i64", "i64", Some("number"), LiteralForm::Signed),
        Scalar::U53 => row("u64", "u64", Some("number"), LiteralForm::Unsigned),
        Scalar::I64 => row("i64", "i64", None, LiteralForm::Signed),
        Scalar::U64 => row("u64", "u64", None, LiteralForm::Unsigned),
        Scalar::F64 => row("f64", "f64", None, LiteralForm::Float),
        Scalar::Path => row("::std::path::PathBuf", "PathBuf", None, LiteralForm::Path),
        Scalar::Json => row(
            "::serde_json::Value",
            "serde_json::Value",
            Some("unknown"),
            LiteralForm::Json,
        ),
    }
}

/// The TypeScript a schema maps to, in the cases where `ts_rs` reading the
/// Rust alone would get it wrong. The leaf answer is the table's `ts` column;
/// what is here is the way a list or a nullable wraps it.
fn ts_override(schema: &Schema) -> Option<String> {
    match schema {
        Schema::Array { array } => ts_override(array).map(|inner| format!("Array<{inner}>")),
        Schema::Nullable { nullable } => {
            ts_override(nullable).map(|inner| format!("{inner} | null"))
        }
        Schema::Named(name) => Scalar::from_keyword(name)
            .and_then(|scalar| scalar_target(scalar).ts)
            .map(ToOwned::to_owned),
    }
}

/// The `#[ts(…)]` attributes one field needs, given that its owner is
/// exported.
///
/// A field's key needs nothing said about it: the generated Rust identifier
/// is the manifest's field name, and `ts_rs` strips the `r#` from a raw one.
/// What it cannot work out for itself is the mapping above, and which keys
/// may be absent — an absent-able key is `key?: T` and not `key: T | null`,
/// a distinction the manifest makes and a Rust `Option` does not.
fn ts_field_attrs(field: &Field) -> TokenStream {
    if field.flatten {
        // A flattened field has no key of its own, and `ts_rs` rejects both
        // of these on one. Its type is a shape, never a scalar, so there is
        // nothing here it would have wanted.
        return TokenStream::new();
    }
    let base = ts_override(&field.ty);
    match (field.optional, field.nullable, base) {
        (true, _, Some(ts)) => quote! { #[ts(optional, type = #ts)] },
        (true, _, None) => quote! { #[ts(optional)] },
        (false, true, Some(ts)) => {
            let nullable = format!("{ts} | null");
            quote! { #[ts(type = #nullable)] }
        }
        (false, false, Some(ts)) => quote! { #[ts(type = #ts)] },
        (false, _, None) => TokenStream::new(),
    }
}

/// The whole expansion, in the order it reads best: what the contract is,
/// what its ops are, what can go wrong, and what shapes travel.
pub(crate) fn contract(
    manifest: &Manifest,
    dir: &Directory,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<TokenStream> {
    let types = manifest.type_map();

    let header = header(manifest, span);
    let ops = match manifest.role {
        // A vocabulary has no producer, so there is no registration for
        // either list to feed and nothing to put in them.
        Role::Vocabulary => TokenStream::new(),
        Role::Contract => ops(manifest, span)?,
    };
    let stubs = match (manifest.role, bindings.runtime) {
        (Role::Contract, true) => runtime(manifest, bindings, span)?,
        _ => TokenStream::new(),
    };
    let errors = errors(manifest, span);
    let shapes = manifest
        .types
        .iter()
        .map(|def| shape(def, manifest, dir, &types, bindings, span))
        .collect::<syn::Result<Vec<_>>>()?;
    let tracker = rebuild_tracker(dir, span);

    Ok(quote! {
        #header
        #ops
        #errors
        #(#shapes)*
        #stubs
        #tracker
    })
}

fn header(manifest: &Manifest, span: Span) -> TokenStream {
    let name = LitStr::new(&manifest.contract, span);
    let version = manifest.version;
    let contract_docs = docs(&manifest.description);
    let version_docs = doc_lines(&[
        format!(
            "The major version of `{}` this expansion describes.",
            manifest.contract
        ),
        String::new(),
        "One integer, additive within it: a breaking change is a new version and a new manifest, \
         never an edit to this one."
            .into(),
    ]);
    quote! {
        #contract_docs
        pub const CONTRACT: &str = #name;

        #version_docs
        pub const VERSION: u32 = #version;
    }
}

fn ops(manifest: &Manifest, span: Span) -> syn::Result<TokenStream> {
    let errors = manifest.error_map();
    let mut consts = Vec::new();
    let mut names = Vec::new();
    let mut kinds = Vec::new();

    for op in &manifest.ops {
        let ident = ident(
            &peerline_contract_spec::const_name("OP", op.short_name(&manifest.contract)),
            span,
        )?;
        let literal = LitStr::new(&op.name, span);
        let prose = docs(&op.description);
        let summary = doc_lines(&op_summary(op, &errors));
        consts.push(quote! {
            #prose
            #summary
            pub const #ident: &str = #literal;
        });
        names.push(quote! { #ident });
        let kind = match op.kind {
            OpKind::Unary => quote!(::peerline_contract::spec::OpKind::Unary),
            OpKind::Stream => quote!(::peerline_contract::spec::OpKind::Stream),
        };
        kinds.push(quote! { (#ident, #kind) });
    }

    let ops_docs = doc_lines(&[
        "Every op of this contract, in the manifest's order.".into(),
        String::new(),
        "This is the list a producer hands to `Produced::with_ops`, so an op missing from it is \
         an op no consumer can discover."
            .into(),
    ]);
    let kinds_docs = doc_lines(&[
        "Every op paired with whether it is unary or a stream.".into(),
        String::new(),
        "Unary and stream are different peerline calls: a producer that registers a stream op as \
         unary fails on the first call with a kind mismatch, and no argument or result schema \
         would have caught it. This table is the manifest's answer, in a form a registration test \
         can check against."
            .into(),
    ]);

    // The declaration every producer of this contract wrote out by hand, and
    // wrote out the same way. `Produced::with_ops(CONTRACT, VERSION,
    // OPS.iter().copied())` is three facts the expansion already holds, so a
    // producer restating them is three chances to hand a coordinator a name,
    // a version or an op list that is not the one it registers handlers for.
    //
    // Unconditional, unlike the runtime stubs below it, and for a reason
    // worth stating: this names `::peerline_contract`, which the expansion
    // *already* requires — `OP_KINDS` above is typed
    // `::peerline_contract::spec::OpKind`. Any crate that can compile the op
    // table can compile this, so it costs no call site a new dependency. The
    // stubs are different: they name `::peerline`, which nothing else in the
    // expansion does.
    let produced_docs = doc_lines(&[
        "This contract as a producer declares it, with its full op list.".into(),
        String::new(),
        "What a `Service::produces` returns. Written here rather than at each producer because \
         the contract name, the version and the op list are all in this expansion already, and a \
         producer restating them is a producer that can restate one of them wrongly."
            .into(),
    ]);

    Ok(quote! {
        #(#consts)*

        #ops_docs
        pub const OPS: &[&str] = &[#(#names),*];

        #kinds_docs
        pub const OP_KINDS: &[(&str, ::peerline_contract::spec::OpKind)] = &[#(#kinds),*];

        #produced_docs
        #[must_use]
        pub fn produced() -> ::peerline_contract::Produced {
            ::peerline_contract::Produced::with_ops(CONTRACT, VERSION, OPS.iter().copied())
        }
    })
}

/// The one part of an op's documentation that is *not* the manifest's prose:
/// a line naming the Rust spellings a caller needs. The manifest cannot write
/// it, because the names are this generator's.
fn op_summary(op: &OpDef, errors: &BTreeMap<&str, &ErrorDef>) -> Vec<String> {
    let mut parts = vec![format!("Kind: {}.", op.kind)];
    parts.push(match &op.args {
        Some(args) => format!("Args: `{}`.", rust_type_name(args)),
        None => "Args: none.".into(),
    });
    match op.kind {
        OpKind::Unary => parts.push(match &op.result {
            Some(result) => format!("Result: `{}`.", rust_type_name(result)),
            None => "Result: empty.".into(),
        }),
        OpKind::Stream => {
            if let Some(item) = &op.item {
                parts.push(format!("Items: `{}`.", rust_type_name(item)));
            }
        }
    }
    let mut lines = vec![String::new(), parts.join(" ")];
    if !op.errors.is_empty() {
        let raised: Vec<String> = op
            .errors
            .iter()
            .filter_map(|name| errors.get(name.as_str()))
            .map(|e| format!("`{}`", e.const_name()))
            .collect();
        lines.push(format!("Raises: {}.", raised.join(", ")));
    }
    lines
}

// ---------------------------------------------------------------------------
// The runtime stubs
// ---------------------------------------------------------------------------

/// The producer trait, the registration that wires it, and the consumer's
/// client — the three things every implementor of a contract wrote by hand.
///
/// # What this is for
///
/// `OP_KINDS` was added because a producer that registers a stream op with
/// `on_request` answers a `call_stream` with a kind mismatch at runtime, on
/// the first call, in a way no argument or result schema catches. But a table
/// only helps whoever writes the test that reads it, and the hand-wiring it
/// was meant to guard — eight `peer.on_request(ops::OP_X, …)` lines and one
/// `on_stream_request` — is exactly the code nobody writes a test for. So the
/// registration is generated from the same table instead: there is no line at
/// which a producer *could* name the wrong call, because the producer does
/// not name a call at all. The failure `OP_KINDS` was created to catch stops
/// being catchable and starts being unwritable.
///
/// The same argument applies at the other end, one step weaker. A consumer
/// passing `ops::OP_WATCH` to `peer.call` rather than `call_stream` is the
/// mirror image of the same mistake, and the generated client has one method
/// per op with the op's own kind baked into it.
///
/// # Why it is opt-in, and what a call site must have for it
///
/// Everything else this macro emits resolves against `::serde`,
/// `::serde_json` and `::peerline_contract` — the dependencies any crate
/// using the macro has by construction. The stubs do not: they name
/// `::peerline::runtime::Peer`, `::peerline::runtime::StreamSender` and
/// `::peerline::wire::RpcError`, and a crate that only wants the *types* of a
/// contract — a dashboard, a CLI, a crate that re-encodes a manifest's shapes
/// onto some other wire — has no peer runtime and should not grow one.
///
/// A proc macro has no `$crate`, so there is no way to emit a path that
/// resolves wherever this crate happens to be named; the path is whatever the
/// *invoking* crate calls its dependency. That is a constraint, not a choice,
/// and the honest response to it is to make the clause explicit rather than
/// to emit paths that may or may not resolve. So:
///
/// ```ignore
/// peerline_contract::contract!("demo.thing/2" in "contracts", runtime);
/// ```
///
/// and the invoking crate needs `peerline = { version = "…", features =
/// ["runtime"] }` under exactly that name. `::peerline` rather than
/// `::peerline_host` because `peerline-host` does not re-export `Peer` and
/// should not have to: a producer of a contract needs a peer, not a host, and
/// the two crates that implement contracts without being hosts would
/// otherwise link a transport stack to get at a type alias.
///
/// # The three generated names
///
/// For `demo.thing`: the trait `DemoThingService`, the function
/// `register_demo_thing_handlers`, and the client `DemoThingClient`; plus one
/// `…Items` sink per stream op. A manifest that declares a shape by one of
/// those names is rejected here rather than left to collide in the expansion,
/// because the collision otherwise lands as a duplicate-definition error on a
/// line that mentions neither the manifest nor the clause.
fn runtime(manifest: &Manifest, bindings: &Bindings, span: Span) -> syn::Result<TokenStream> {
    let types = manifest.type_map();
    let errors = manifest.error_map();
    let base = pascal_case(&manifest.contract);

    let trait_name = reserved(
        &format!("{base}Service"),
        "the producer trait",
        &types,
        span,
    )?;
    let client_name = reserved(&format!("{base}Client"), "the client", &types, span)?;
    let register_name = format_ident!(
        "register_{}_handlers",
        snake_case(&manifest.contract),
        span = span
    );

    let mut methods = Vec::new();
    let mut registrations = Vec::new();
    let mut sinks = Vec::new();
    let mut calls = Vec::new();

    for op in &manifest.ops {
        let short = op.short_name(&manifest.contract);
        let konst = ident(&peerline_contract_spec::const_name("OP", short), span)?;
        let method = ident(&snake_case(short), span)?;
        let prose = docs(&op.description);
        let summary = doc_lines(&op_summary(op, &errors));

        // An op with no `args` still travels as one: peerline rejects params
        // that are not a JSON object, so the empty argument is an empty
        // object rather than nothing. A map is what both ends of that spell:
        // it decodes from `{}` on the way in and encodes to `{}` on the way
        // out, where a `()` would insist on `null` and be refused.
        let args_ty = match &op.args {
            Some(args) => rust_type(args, bindings, span),
            None => quote! { ::serde_json::Map<::std::string::String, ::serde_json::Value> },
        };
        let (takes, passes, sends) = match &op.args {
            Some(_) => (
                quote! { , args: #args_ty },
                quote! { args },
                quote! { , args: &#args_ty },
            ),
            None => (TokenStream::new(), TokenStream::new(), TokenStream::new()),
        };
        let client_args = match &op.args {
            Some(_) => quote! { args },
            None => quote! { &::serde_json::Map::new() },
        };

        match op.kind {
            OpKind::Unary => {
                // Absent `result` means the op answers nothing at all, every
                // time — which is `()`, encoded as JSON null, and not an
                // absent value a caller has to test for.
                let result_ty = match &op.result {
                    Some(result) => rust_type(result, bindings, span),
                    None => quote! { () },
                };
                methods.push(quote! {
                    #prose
                    #summary
                    fn #method(&self #takes)
                    -> impl ::core::future::Future<
                        Output = ::core::result::Result<#result_ty, ::peerline::wire::RpcError>,
                    > + ::core::marker::Send;
                });
                registrations.push(quote! {
                    {
                        let imp = ::std::sync::Arc::clone(&imp);
                        peer.on_request(#konst, move |args: #args_ty| {
                            let imp = ::std::sync::Arc::clone(&imp);
                            async move { imp.#method(#passes).await }
                        });
                    }
                });
                calls.push(quote! {
                    #prose
                    #summary
                    pub async fn #method(&self #sends)
                    -> ::core::result::Result<#result_ty, ::peerline::runtime::Error> {
                        self.peer.call(#konst, #client_args).await
                    }
                });
            }
            OpKind::Stream => {
                let Some(item) = &op.item else {
                    return Err(syn::Error::new(
                        span,
                        format!("`{}` is a stream op with no `item`", op.name),
                    ));
                };
                let item_ty = rust_type(item, bindings, span);
                let sink = reserved(
                    &format!("{}Items", pascal_case(short)),
                    &format!("the item sink for `{}`", op.name),
                    &types,
                    span,
                )?;
                let sink_docs = doc_lines(&[
                    format!("Where a producer puts the items of `{}`.", op.name),
                    String::new(),
                    format!(
                        "A `StreamSender` with the manifest's item type on it. The bare sender \
                         takes anything serialisable, so a producer that sent the wrong shape — \
                         or the right shape from the wrong op — would be found by whatever \
                         decoded it, at the consumer. Here it is `{}` or it is a compile error.",
                        rust_type_name(item)
                    ),
                ]);
                sinks.push(quote! {
                    #sink_docs
                    pub struct #sink(::peerline::runtime::StreamSender);

                    impl #sink {
                        #[doc = " Put the manifest's item type on a raw sender."]
                        #[doc = ""]
                        #[doc = " The registration below does this; it is public so that a test can \
                                 drive the producer trait without a peer."]
                        #[must_use]
                        pub fn new(sender: ::peerline::runtime::StreamSender) -> Self {
                            Self(sender)
                        }

                        #[doc = " Emit one item."]
                        #[doc = ""]
                        #[doc = " Fails only when the consumer has gone — it cancelled the stream, \
                                 or the connection closed. That is not an error to raise out of the \
                                 op: there is nobody left to raise it to."]
                        pub fn send(&self, item: &#item_ty)
                        -> ::core::result::Result<(), ::peerline::runtime::Error> {
                            self.0.send_item(item)
                        }

                        #[doc = " Whether the consumer has already gone."]
                        #[must_use]
                        pub fn is_cancelled(&self) -> bool {
                            self.0.is_cancelled()
                        }

                        #[doc = " Resolves when the consumer drops its end."]
                        #[doc = ""]
                        #[doc = " A producer reading from somewhere expensive selects on this, so \
                                 that a cancelled stream stops the work rather than only the \
                                 sending."]
                        pub async fn cancelled(&self) {
                            self.0.cancelled().await;
                        }

                        #[doc = " The sender underneath, for the things this wrapper does not say."]
                        #[must_use]
                        pub fn sender(&self) -> &::peerline::runtime::StreamSender {
                            &self.0
                        }
                    }
                });
                methods.push(quote! {
                    #prose
                    #summary
                    fn #method(&self #takes, items: #sink)
                    -> impl ::core::future::Future<
                        Output = ::core::result::Result<(), ::peerline::wire::RpcError>,
                    > + ::core::marker::Send;
                });
                registrations.push(quote! {
                    {
                        let imp = ::std::sync::Arc::clone(&imp);
                        peer.on_stream_request(
                            #konst,
                            move |args: #args_ty, sender: ::peerline::runtime::StreamSender| {
                                let imp = ::std::sync::Arc::clone(&imp);
                                async move { imp.#method(#passes, #sink::new(sender)).await }
                            },
                        );
                    }
                });
                calls.push(quote! {
                    #prose
                    #summary
                    pub fn #method(&self #sends)
                    -> ::core::result::Result<
                        ::peerline::runtime::StreamReceiver<#item_ty>,
                        ::peerline::runtime::Error,
                    > {
                        self.peer.call_stream(#konst, #client_args)
                    }
                });
            }
        }
    }

    let trait_docs = doc_lines(&[
        format!("What a producer of `{}` answers.", manifest.contract),
        String::new(),
        "One method per op, taking and returning the manifest's own shapes. A stream op takes an \
         item sink instead of returning a result, because that is what a stream is: the items go \
         out as they are found, and the return value says only whether it ended well."
            .into(),
        String::new(),
        "An implementation writes `async fn` for each of these; the future is spelled out here \
         rather than as `async fn` in the trait so that it can be required to be `Send`, which is \
         what lets the registration hand it to a peer."
            .into(),
    ]);
    let register_docs = doc_lines(&[
        format!(
            "Register every op of `{}` on `peer`, each with the kind the manifest declares.",
            manifest.contract
        ),
        String::new(),
        "This is the whole reason the trait exists. Wired by hand, a stream op registered with \
         `on_request` answers a `call_stream` with a kind mismatch at run time, on the first \
         call, and no argument or result schema would have caught it. Here there is no call to \
         name: the kind comes from the same manifest the op constant does."
            .into(),
        String::new(),
        "Call it from `Service::register`, which runs once per connection — so the `Arc` is \
         cloned per connection and the implementation is shared, not copied."
            .into(),
    ]);
    let client_docs = doc_lines(&[
        format!("A typed caller for `{}`.", manifest.contract),
        String::new(),
        "One method per op: `call` for a unary op, `call_stream` for a stream one, with the \
         argument and result shapes the manifest declares. What it replaces is a consumer \
         passing a bare `&str` op name and naming the result type at the call site, where \
         nothing checks either against the contract."
            .into(),
    ]);

    Ok(quote! {
        #(#sinks)*

        #trait_docs
        pub trait #trait_name: ::core::marker::Send + ::core::marker::Sync + 'static {
            #(#methods)*
        }

        #register_docs
        pub fn #register_name<I: #trait_name>(
            peer: &::peerline::runtime::Peer,
            imp: ::std::sync::Arc<I>,
        ) {
            #(#registrations)*
        }

        #client_docs
        #[derive(::core::clone::Clone)]
        pub struct #client_name {
            peer: ::peerline::runtime::Peer,
        }

        impl #client_name {
            #[doc = " Call this contract over an established peer."]
            #[doc = ""]
            #[doc = " A `Peer` is a cheap handle, so this takes one by value rather than \
                     borrowing: a client that outlives the expression that made it is the \
                     normal case."]
            #[must_use]
            pub fn new(peer: ::peerline::runtime::Peer) -> Self {
                Self { peer }
            }

            #[doc = " The peer underneath, for ops this contract does not declare."]
            #[must_use]
            pub fn peer(&self) -> &::peerline::runtime::Peer {
                &self.peer
            }

            #(#calls)*
        }
    })
}

/// A name this expansion needs for itself, checked against the manifest's own.
///
/// The manifest names shapes and this generator names three more things in
/// the same namespace. A collision is a real possibility — `…Client` and
/// `…Items` are ordinary words — and left alone it lands as a
/// duplicate-definition error on a generated line, which says nothing about
/// the manifest or the clause that asked for it.
fn reserved(
    name: &str,
    what: &str,
    types: &BTreeMap<&str, &TypeDef>,
    span: Span,
) -> syn::Result<Ident> {
    if types.contains_key(name) {
        return Err(syn::Error::new(
            span,
            format!(
                "the `runtime` clause generates `{name}` for {what}, and this manifest already \
                 declares a shape called `{name}`; rename the shape, or drop the clause"
            ),
        ));
    }
    Ok(format_ident!("{}", name, span = span))
}

/// `thread.start` to `thread_start`: the manifest's own spelling as a Rust
/// member name.
///
/// Routed through [`screaming_case`] rather than written again, so that the
/// dots-and-dashes rule has one definition and a name that is `THREAD_START`
/// as a constant is `thread_start` as a method.
fn snake_case(name: &str) -> String {
    screaming_case(name).to_lowercase()
}

fn errors(manifest: &Manifest, span: Span) -> TokenStream {
    let consts: Vec<TokenStream> = manifest
        .errors
        .iter()
        .map(|err| {
            let ident = format_ident!("{}", err.const_name(), span = span);
            let code = err.code;
            let prose = docs(&err.description);
            let carried = match &err.data {
                Some(data) => doc_lines(&[
                    String::new(),
                    format!("Carried in `RpcError::data` as `{}`.", rust_type_name(data)),
                ]),
                None => TokenStream::new(),
            };
            quote! {
                #prose
                #carried
                pub const #ident: i32 = #code;
            }
        })
        .collect();
    quote! { #(#consts)* }
}

fn shape(
    def: &TypeDef,
    owner: &Manifest,
    dir: &Directory,
    types: &BTreeMap<&str, &TypeDef>,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<TokenStream> {
    match def {
        TypeDef::Struct {
            name,
            description,
            fields,
        } => {
            let ty = ident(name, span)?;
            let prose = docs(description);
            let exported = bindings.is_exported(name);
            let decls = fields
                .iter()
                .map(|f| field_decl(&ty, "", f, bindings, span, Visibility::Public, exported))
                .collect::<syn::Result<Vec<_>>>()?;
            let derives = comparison_derives(support_of(def, owner, dir));
            let ts = ts_derive(exported);
            let ctor = constructor(&ty, name, fields, types, bindings, span)?;
            let defaults = default_fns(&ty, "", fields, types, bindings, span)?;
            let default_impl = default_impl(&ty, name, fields, types, bindings, span)?;
            Ok(quote! {
                #prose
                #[derive(::core::fmt::Debug, ::core::clone::Clone, #derives ::serde::Serialize, ::serde::Deserialize)]
                #ts
                pub struct #ty { #(#decls),* }
                #defaults
                #ctor
                #default_impl
            })
        }
        TypeDef::Enum {
            name,
            description,
            variants,
        } => {
            let ty = ident(name, span)?;
            let prose = docs(description);
            let exported = bindings.is_exported(name);
            let arms: Vec<TokenStream> = variants
                .iter()
                .map(|v| {
                    let arm = format_ident!("{}", pascal_case(&v.name), span = span);
                    let wire = LitStr::new(v.wire(), span);
                    let prose = docs(&v.description);
                    let ts = ts_variant_rename(exported, &wire);
                    quote! {
                        #prose
                        #[serde(rename = #wire)]
                        #ts
                        #arm
                    }
                })
                .collect();
            let ts = ts_derive(exported);
            // No `#[non_exhaustive]`: a generated set is closed, and the module
            // docs above say why.
            Ok(quote! {
                #prose
                #[derive(
                    ::core::fmt::Debug, ::core::clone::Clone, ::core::marker::Copy,
                    ::core::cmp::PartialEq, ::core::cmp::Eq, ::core::hash::Hash,
                    ::serde::Serialize, ::serde::Deserialize
                )]
                #ts
                pub enum #ty { #(#arms),* }
            })
        }
        TypeDef::Union {
            name,
            description,
            tagging,
            tag,
            variants,
        } => {
            let ty = ident(name, span)?;
            let prose = docs(description);
            let exported = bindings.is_exported(name);
            let mut arms = Vec::new();
            let mut ctors = Vec::new();
            let mut defaults = Vec::new();
            for v in variants {
                let arm = format_ident!("{}", pascal_case(&v.name), span = span);
                let wire = LitStr::new(v.wire(), span);
                let prose = docs(&v.description);
                let ts_rename = ts_variant_rename(exported, &wire);
                match (&v.payload, v.fields.is_empty()) {
                    (Some(payload), _) => {
                        let inner = rust_type(payload, bindings, span);
                        arms.push(quote! {
                            #prose
                            #[serde(rename = #wire)]
                            #ts_rename
                            #arm(#inner)
                        });
                    }
                    (None, true) => {
                        arms.push(quote! {
                            #prose
                            #[serde(rename = #wire)]
                            #ts_rename
                            #arm
                        });
                    }
                    (None, false) => {
                        let decls = v
                            .fields
                            .iter()
                            .map(|f| {
                                field_decl(
                                    &ty,
                                    &format!("{}_", v.name),
                                    f,
                                    bindings,
                                    span,
                                    Visibility::Inherited,
                                    exported,
                                )
                            })
                            .collect::<syn::Result<Vec<_>>>()?;
                        arms.push(quote! {
                            #prose
                            #[serde(rename = #wire)]
                            #ts_rename
                            #arm { #(#decls),* }
                        });
                        defaults.push(default_fns(
                            &ty,
                            &format!("{}_", v.name),
                            &v.fields,
                            types,
                            bindings,
                            span,
                        )?);
                    }
                }
                ctors.extend(variant_members(&ty, &arm, v, types, bindings, span)?);
            }
            let derives = comparison_derives(support_of(def, owner, dir));
            let impl_block = if ctors.is_empty() {
                TokenStream::new()
            } else {
                quote! { impl #ty { #(#ctors)* } }
            };
            let container = match tagging {
                Tagging::Internal => {
                    let tag_literal = LitStr::new(
                        tag.as_deref().expect("validation requires an internal tag"),
                        span,
                    );
                    let ts_tag = if exported {
                        quote! { #[ts(tag = #tag_literal)] }
                    } else {
                        TokenStream::new()
                    };
                    quote! { #[serde(tag = #tag_literal)] #ts_tag }
                }
                // Externally tagged is serde's default for an enum: the arm's
                // name is the key and its payload is the value.
                Tagging::External => TokenStream::new(),
            };
            let ts = ts_derive(exported);
            // Closed for the same reason an enum is; see the module docs.
            Ok(quote! {
                #prose
                #[derive(::core::fmt::Debug, ::core::clone::Clone, #derives ::serde::Serialize, ::serde::Deserialize)]
                #ts
                #container
                pub enum #ty { #(#arms),* }
                #(#defaults)*
                #impl_block
            })
        }
    }
}

/// The derive that makes a shape a TypeScript type.
///
/// Written as `::ts_rs::TS`, resolved in the *invoking* crate: this crate
/// has no `ts_rs` dependency and neither does `peerline-contract`, so a
/// crate that never writes the clause never links it, and a crate that does
/// declares it for itself like any other dependency.
fn ts_derive(exported: bool) -> TokenStream {
    if exported {
        quote! { #[derive(::ts_rs::TS)] }
    } else {
        TokenStream::new()
    }
}

/// A variant's wire spelling, said to `ts_rs` in its own attribute rather
/// than left to it to read off the serde one.
///
/// `ts_rs` does read serde attributes, but only with its `serde-compat`
/// feature on, and which features a downstream crate enables is not
/// something this expansion can see. The wire spelling of a value is the
/// contract; it must not depend on a Cargo feature.
fn ts_variant_rename(exported: bool, wire: &LitStr) -> TokenStream {
    if exported {
        quote! { #[ts(rename = #wire)] }
    } else {
        TokenStream::new()
    }
}

/// The derives a rung admits, in the order they have always been written.
fn comparison_derives(support: Support) -> TokenStream {
    let mut out = TokenStream::new();
    if support >= Support::Copy {
        out.extend(quote! { ::core::marker::Copy, });
    }
    if support >= Support::Partial {
        out.extend(quote! { ::core::cmp::PartialEq, });
    }
    if support >= Support::Total {
        out.extend(quote! { ::core::cmp::Eq, });
    }
    out
}

fn field_decl(
    owner: &Ident,
    prefix: &str,
    field: &Field,
    bindings: &Bindings,
    span: Span,
    vis: Visibility,
    exported: bool,
) -> syn::Result<TokenStream> {
    let name = ident(&field.name, span)?;
    let ty = rust_type(&field.ty, bindings, span);
    let prose = docs(&field.description);
    let ts = if exported {
        ts_field_attrs(field)
    } else {
        TokenStream::new()
    };
    // A struct's fields are `pub`; an enum variant's inherit the enum's
    // visibility and may not be spelled.
    let vis = match vis {
        Visibility::Public => quote! { pub },
        Visibility::Inherited => TokenStream::new(),
    };
    if field.flatten {
        // No key of its own: the shape's keys are written beside this
        // field's siblings, which is what the wire already does.
        return Ok(quote! {
            #prose
            #[serde(flatten)]
            #ts
            #vis #name: #ty
        });
    }
    if field.default.is_some() {
        let getter = default_fn_ident(owner, prefix, &field.name);
        let path = LitStr::new(&format!("{owner}::{getter}"), span);
        return Ok(quote! {
            #prose
            #[serde(default = #path)]
            #ts
            #vis #name: #ty
        });
    }
    if field.optional {
        // Absent, not null: `skip_serializing_if` is what makes an omitted
        // optional field omitted on the way out too, which is the form the
        // manifest describes and the form another language's decoder expects.
        return Ok(quote! {
            #prose
            #[serde(default, skip_serializing_if = "::core::option::Option::is_none")]
            #ts
            #vis #name: ::core::option::Option<#ty>
        });
    }
    if field.nullable {
        // Null, not absent: no `skip_serializing_if`, so the key is always
        // written. `default` is still here because a decoder that tolerates
        // less than it emits helps nobody.
        return Ok(quote! {
            #prose
            #[serde(default)]
            #ts
            #vis #name: ::core::option::Option<#ty>
        });
    }
    Ok(quote! {
        #prose
        #ts
        #vis #name: #ty
    })
}

/// Whether a generated field may be spelled `pub`.
#[derive(Clone, Copy)]
enum Visibility {
    /// A struct field.
    Public,
    /// An enum variant's field, which takes the enum's own visibility.
    Inherited,
}

fn default_fn_ident(owner: &Ident, prefix: &str, field: &str) -> Ident {
    format_ident!(
        "default_{prefix}{field}",
        span = owner.span(),
        prefix = prefix,
        field = field
    )
}

/// One function per defaulted field, because that is what serde's `default`
/// attribute takes: a path to something that produces the value.
fn default_fns(
    owner: &Ident,
    prefix: &str,
    fields: &[Field],
    types: &BTreeMap<&str, &TypeDef>,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<TokenStream> {
    let mut fns = Vec::new();
    for field in fields {
        let Some(value) = &field.default else {
            continue;
        };
        let name = default_fn_ident(owner, prefix, &field.name);
        let ty = rust_type(&field.ty, bindings, span);
        let expr = default_expr(value, &field.ty, types, bindings, span)?;
        let prose = doc_lines(&[format!(
            "What the manifest says an absent `{}` means.",
            field.name
        )]);
        fns.push(quote! {
            #prose
            #[doc(hidden)]
            #[must_use]
            pub fn #name() -> #ty { #expr }
        });
    }
    if fns.is_empty() {
        return Ok(TokenStream::new());
    }
    Ok(quote! { impl #owner { #(#fns)* } })
}

/// A shape every one of whose fields has a value when nobody supplied one has
/// a canonical empty form, and that is what `Default` is.
///
/// Written out rather than derived, because a derive would use each Rust
/// type's own default and the manifest's answer is not always that — an
/// absent `include_history` is `true`, and `bool::default()` is not.
fn default_impl(
    ty: &Ident,
    name: &str,
    fields: &[Field],
    types: &BTreeMap<&str, &TypeDef>,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<TokenStream> {
    if fields.iter().any(Field::is_required) {
        return Ok(TokenStream::new());
    }
    let mut inits = Vec::new();
    for field in fields {
        let member = ident(&field.name, span)?;
        let value = unspecified_value(field, types, bindings, span)?;
        inits.push(quote! { #member: #value });
    }
    let prose = doc_lines(&[format!("A `{name}` in which nothing has been said.",)]);
    Ok(quote! {
        #prose
        impl ::core::default::Default for #ty {
            fn default() -> Self {
                Self { #(#inits),* }
            }
        }
    })
}

/// The value of a field nobody supplied: absent, null, or the manifest's
/// default.
fn unspecified_value(
    field: &Field,
    types: &BTreeMap<&str, &TypeDef>,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<TokenStream> {
    match &field.default {
        Some(value) => default_expr(value, &field.ty, types, bindings, span),
        None => Ok(quote! { ::core::option::Option::None }),
    }
}

/// The manifest's JSON default, as the Rust value it describes.
///
/// Validation has already checked the JSON against the declared type, so
/// every branch a manifest can reach is here and none of them can fail on the
/// value's shape.
fn default_expr(
    value: &Value,
    schema: &Schema,
    types: &BTreeMap<&str, &TypeDef>,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<TokenStream> {
    match schema {
        Schema::Nullable { nullable } => {
            if value.is_null() {
                Ok(quote! { ::core::option::Option::None })
            } else {
                let inner = default_expr(value, nullable, types, bindings, span)?;
                Ok(quote! { ::core::option::Option::Some(#inner) })
            }
        }
        Schema::Array { array } => {
            let items = value.as_array().expect("validated as a list");
            if items.is_empty() {
                return Ok(quote! { ::std::vec::Vec::new() });
            }
            let elements = items
                .iter()
                .map(|item| default_expr(item, array, types, bindings, span))
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(quote! { ::std::vec::Vec::from([#(#elements),*]) })
        }
        Schema::Named(name) => {
            if let Some(scalar) = Scalar::from_keyword(name) {
                return Ok(scalar_default(value, scalar));
            }
            let path = bindings.type_path(name, span);
            match types.get(name.as_str()).copied() {
                Some(TypeDef::Enum { variants, .. }) => {
                    let text = value.as_str().expect("validated as an enum value");
                    let variant = variants
                        .iter()
                        .find(|v| v.wire() == text)
                        .expect("validated as a declared value");
                    let arm = format_ident!("{}", pascal_case(&variant.name), span = span);
                    Ok(quote! { #path::#arm })
                }
                Some(TypeDef::Struct { fields, .. }) => {
                    let object = value.as_object().expect("validated as an object");
                    let mut inits = Vec::new();
                    for field in fields {
                        let member = ident(&field.name, span)?;
                        let init = match object.get(&field.name) {
                            Some(inner) => {
                                let expr = default_expr(inner, &field.ty, types, bindings, span)?;
                                if field.optional || field.nullable {
                                    quote! { ::core::option::Option::Some(#expr) }
                                } else {
                                    expr
                                }
                            }
                            None => unspecified_value(field, types, bindings, span)?,
                        };
                        inits.push(quote! { #member: #init });
                    }
                    Ok(quote! { #path { #(#inits),* } })
                }
                // Validation rejects a default on a union and on an imported
                // shape, so neither reaches here.
                _ => Err(syn::Error::new(
                    span,
                    format!("`{name}` cannot carry a default"),
                )),
            }
        }
    }
}

/// The manifest's default for a scalar field, as the Rust expression it is.
///
/// Matches on the table's `literal` column rather than on the keyword: seven
/// of the eleven are a bare numeric literal differing only in which
/// `serde_json` accessor reads them, and naming the six real answers is what
/// stops a twelfth keyword quietly acquiring a seventh.
fn scalar_default(value: &Value, scalar: Scalar) -> TokenStream {
    match scalar_target(scalar).literal {
        LiteralForm::Bool => {
            let flag = value.as_bool().expect("validated as a bool");
            quote! { #flag }
        }
        LiteralForm::Text => {
            let text = value.as_str().expect("validated as a string");
            quote! { ::std::string::String::from(#text) }
        }
        LiteralForm::Path => {
            let text = value.as_str().expect("validated as a string");
            quote! { ::std::path::PathBuf::from(#text) }
        }
        LiteralForm::Json => {
            // The only form whose value has no Rust literal, so it is
            // written as the JSON it is and parsed once. Validation accepted
            // anything here, so nothing but the parse could fail, and the
            // text came out of a parsed document.
            let text = value.to_string();
            quote! {
                ::serde_json::from_str(#text)
                    .expect("the manifest's default is JSON this file was parsed from")
            }
        }
        LiteralForm::Float => {
            let number = value.as_f64().expect("validated as a number");
            let literal = Literal::f64_unsuffixed(number);
            quote! { #literal }
        }
        LiteralForm::Signed => {
            let number = value.as_i64().expect("validated as an integer");
            let literal = Literal::i64_unsuffixed(number);
            quote! { #literal }
        }
        LiteralForm::Unsigned => {
            let number = value.as_u64().expect("validated as an integer");
            let literal = Literal::u64_unsuffixed(number);
            quote! { #literal }
        }
    }
}

/// A struct's constructor, taking exactly what the shape requires.
///
/// Generated only when there is something to require: a `new()` that takes
/// nothing says less than the literal it replaces, and a shape with nothing
/// required has a `Default` instead.
fn constructor(
    ty: &Ident,
    name: &str,
    fields: &[Field],
    types: &BTreeMap<&str, &TypeDef>,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<Option<TokenStream>> {
    let required: Vec<&Field> = fields.iter().filter(|f| f.is_required()).collect();
    if required.is_empty() {
        return Ok(None);
    }
    let (params, inits) = ctor_parts(&required, fields, types, bindings, span)?;
    let mut lines = vec![format!("A `{name}`, given everything it requires.")];
    lines.extend(rest_note(fields));
    let prose = doc_lines(&lines);
    Ok(Some(quote! {
        impl #ty {
            #prose
            #[must_use]
            #[allow(clippy::too_many_arguments)]
            pub fn new(#(#params),*) -> Self {
                Self { #(#inits),* }
            }
        }
    }))
}

/// What a constructor says about the fields it did not take.
fn rest_note(fields: &[Field]) -> Vec<String> {
    let rest: Vec<&Field> = fields.iter().filter(|f| !f.is_required()).collect();
    if rest.is_empty() {
        return Vec::new();
    }
    let line = if rest.iter().all(|f| f.optional) {
        "Its optional fields start absent.".to_string()
    } else {
        "Its remaining fields start at what the manifest says an unspecified one means.".to_string()
    };
    vec![String::new(), line]
}

/// A union variant's constructor, and — for a variant carrying one whole
/// shape — the accessor that gets it back out.
///
/// The accessor exists because a payload variant is the form a consumer
/// chose in order to keep holding the shape: taking it apart to put it on the
/// stream and rebuilding it at the far end is exactly what the payload form
/// avoids.
fn variant_members(
    ty: &Ident,
    arm: &Ident,
    variant: &UnionVariant,
    types: &BTreeMap<&str, &TypeDef>,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<Vec<TokenStream>> {
    let name = ident(&variant.name, span)?;
    if let Some(payload) = &variant.payload {
        let inner = rust_type(payload, bindings, span);
        let getter = format_ident!("as_{}", variant.name, span = span);
        let ctor_docs = doc_lines(&[format!(
            "The `{}` variant of `{ty}`, carrying one `{}`.",
            variant.name,
            rust_type_name(payload)
        )]);
        let getter_docs = doc_lines(&[format!(
            "The payload, when this is the `{}` variant.",
            variant.name
        )]);
        return Ok(vec![quote! {
            #ctor_docs
            #[must_use]
            pub fn #name(#name: #inner) -> Self { Self::#arm(#name) }

            #getter_docs
            #[must_use]
            pub fn #getter(&self) -> ::core::option::Option<&#inner> {
                match self {
                    Self::#arm(payload) => ::core::option::Option::Some(payload),
                    _ => ::core::option::Option::None,
                }
            }
        }]);
    }
    if variant.fields.is_empty() {
        let prose = doc_lines(&[format!("The `{}` variant of `{ty}`.", variant.name)]);
        return Ok(vec![quote! {
            #prose
            #[must_use]
            pub fn #name() -> Self { Self::#arm }
        }]);
    }
    let required: Vec<&Field> = variant.fields.iter().filter(|f| f.is_required()).collect();
    let (params, inits) = ctor_parts(&required, &variant.fields, types, bindings, span)?;
    let mut lines = vec![format!(
        "The `{}` variant of `{ty}`, given everything it requires.",
        variant.name
    )];
    lines.extend(rest_note(&variant.fields));
    let prose = doc_lines(&lines);
    Ok(vec![quote! {
        #prose
        #[must_use]
        #[allow(clippy::too_many_arguments)]
        pub fn #name(#(#params),*) -> Self {
            Self::#arm { #(#inits),* }
        }
    }])
}

type CtorParts = (Vec<TokenStream>, Vec<TokenStream>);

fn ctor_parts(
    required: &[&Field],
    all: &[Field],
    types: &BTreeMap<&str, &TypeDef>,
    bindings: &Bindings,
    span: Span,
) -> syn::Result<CtorParts> {
    let mut params = Vec::new();
    for field in required {
        let name = ident(&field.name, span)?;
        // A string-ish parameter takes anything convertible, because every
        // call site otherwise writes `.to_string()` at it.
        let param = match &field.ty {
            Schema::Named(n) if Scalar::from_keyword(n) == Some(Scalar::String) => {
                quote! { #name: impl ::core::convert::Into<::std::string::String> }
            }
            Schema::Named(n) if Scalar::from_keyword(n) == Some(Scalar::Path) => {
                quote! { #name: impl ::core::convert::Into<::std::path::PathBuf> }
            }
            other => {
                let ty = rust_type(other, bindings, span);
                quote! { #name: #ty }
            }
        };
        params.push(param);
    }
    let mut inits = Vec::new();
    for field in all {
        let name = ident(&field.name, span)?;
        if !field.is_required() {
            let value = unspecified_value(field, types, bindings, span)?;
            inits.push(quote! { #name: #value });
            continue;
        }
        match &field.ty {
            Schema::Named(n)
                if matches!(Scalar::from_keyword(n), Some(Scalar::String | Scalar::Path)) =>
            {
                inits.push(quote! { #name: #name.into() });
            }
            _ => inits.push(quote! { #name }),
        }
    }
    Ok((params, inits))
}

/// How far up the ladder a shape goes, read off the manifest.
///
/// Not a cosmetic question: a type that can be a `HashMap` key or compared
/// with `assert_eq!` on both halves of a seam is more useful than one that
/// cannot, and what rules it out is an `f64` or a passthrough `json`
/// somewhere in the shape. The manifest already says which, so the derives
/// follow from it rather than from somebody remembering.
///
/// An imported shape is no exception, which is what loading a directory
/// bought: `owner` says whose scope a name is being read in, and a name that
/// resolves into another manifest carries on being read there.
fn support_of(def: &TypeDef, owner: &Manifest, dir: &Directory) -> Support {
    let mut seen = BTreeSet::new();
    type_support(def, owner, dir, &mut seen)
}

fn type_support(
    def: &TypeDef,
    owner: &Manifest,
    dir: &Directory,
    seen: &mut BTreeSet<(String, String)>,
) -> Support {
    match def {
        TypeDef::Enum { .. } => Support::Copy,
        TypeDef::Struct { fields, .. } => fields
            .iter()
            .map(|f| schema_support(&f.ty, owner, dir, seen))
            .min()
            .unwrap_or(Support::Copy),
        TypeDef::Union { variants, .. } => {
            // A union is never `Copy` today and nothing here changes that: a
            // payload variant holds whatever the payload is, and a shape that
            // gained a `Copy` it did not have would be an API change made by
            // a field nobody touched.
            let inner = variants
                .iter()
                .flat_map(|v| {
                    v.fields
                        .iter()
                        .map(|f| &f.ty)
                        .chain(v.payload.iter())
                        .map(|s| schema_support(s, owner, dir, seen))
                        .collect::<Vec<_>>()
                })
                .min()
                .unwrap_or(Support::Total);
            inner.min(Support::Total)
        }
    }
}

fn schema_support(
    schema: &Schema,
    owner: &Manifest,
    dir: &Directory,
    seen: &mut BTreeSet<(String, String)>,
) -> Support {
    match schema {
        // A list is never `Copy`, whatever it holds.
        Schema::Array { array } => schema_support(array, owner, dir, seen).min(Support::Total),
        // An absent-or-null value supports exactly what the value does.
        Schema::Nullable { nullable } => schema_support(nullable, owner, dir, seen),
        Schema::Named(name) => {
            if let Some(scalar) = Scalar::from_keyword(name) {
                return match (scalar.is_exact(), scalar.is_small()) {
                    (true, true) => Support::Copy,
                    (true, false) => Support::Total,
                    (false, _) => Support::Partial,
                };
            }
            // Validation has already insisted that every name resolves, in
            // this manifest or in one it imports from.
            let Some((declaring, def)) = dir.resolve(owner, name) else {
                return Support::Total;
            };
            // A cycle can only be through a list, which the validator
            // insisted on, and a list has already capped the rung.
            if !seen.insert((declaring.id(), name.clone())) {
                return Support::Copy;
            }
            type_support(def, declaring, dir, seen)
        }
    }
}

fn rust_type(schema: &Schema, bindings: &Bindings, span: Span) -> TokenStream {
    match schema {
        Schema::Array { array } => {
            let inner = rust_type(array, bindings, span);
            quote! { ::std::vec::Vec<#inner> }
        }
        Schema::Nullable { nullable } => {
            let inner = rust_type(nullable, bindings, span);
            quote! { ::core::option::Option<#inner> }
        }
        Schema::Named(name) => match Scalar::from_keyword(name) {
            // The table's `rust` column, parsed back into tokens. Written as
            // text so that one row can carry the type, the doc spelling, the
            // TypeScript and the literal form together; a `quote!` per arm
            // would be a fourth match on the same eleven keywords.
            Some(scalar) => scalar_target(scalar)
                .rust
                .parse()
                .expect("the table's Rust spellings are types this crate wrote"),
            None => bindings.type_path(name, span),
        },
    }
}

/// The same mapping, rendered for a doc comment rather than for the compiler.
fn rust_type_name(schema: &Schema) -> String {
    match schema {
        Schema::Array { array } => format!("Vec<{}>", rust_type_name(array)),
        Schema::Nullable { nullable } => format!("Option<{}>", rust_type_name(nullable)),
        Schema::Named(name) => match Scalar::from_keyword(name) {
            Some(scalar) => scalar_target(scalar).doc.to_owned(),
            None => name.clone(),
        },
    }
}

/// The manifest's prose, as doc comments.
///
/// Line by line rather than as one blob, so a paragraph break in the manifest
/// is a paragraph break in the rendered docs.
fn docs(prose: &Prose) -> TokenStream {
    let lines: Vec<String> = prose.lines().iter().map(|l| (*l).to_string()).collect();
    doc_lines(&lines)
}

fn doc_lines(lines: &[String]) -> TokenStream {
    let attrs: Vec<TokenStream> = lines
        .iter()
        .map(|line| {
            let text = if line.is_empty() {
                String::new()
            } else {
                format!(" {line}")
            };
            quote! { #[doc = #text] }
        })
        .collect();
    quote! { #(#attrs)* }
}

/// The dependency edges that make a manifest edit a rebuild.
///
/// One per file in the directory, not one for the manifest asked for: every
/// file was read, so an edit to any of them can change or invalidate this
/// expansion. See the crate docs, including the one case this cannot cover.
fn rebuild_tracker(dir: &Directory, span: Span) -> TokenStream {
    let edges = dir.files().iter().map(|path| {
        let literal = LitStr::new(&path.to_string_lossy(), span);
        quote! { const _: &[u8] = include_bytes!(#literal); }
    });
    quote! { #(#edges)* }
}

/// A manifest name as a Rust identifier.
///
/// A name that is a Rust keyword is not a manifest error — the manifest is
/// not a Rust file, and another target would take it without complaint — so
/// it becomes a raw identifier, which serialises under its own spelling and
/// so keeps the wire form the manifest declared. The five that cannot even be
/// raw are reported against this target by name.
fn ident(name: &str, span: Span) -> syn::Result<Ident> {
    if let Ok(mut id) = syn::parse_str::<Ident>(name) {
        id.set_span(span);
        return Ok(id);
    }
    if matches!(name, "crate" | "self" | "Self" | "super" | "_") {
        return Err(syn::Error::new(
            span,
            format!(
                "`{name}` cannot be spelled as a Rust identifier, even raw; Rust needs it \
                 renamed in the manifest"
            ),
        ));
    }
    Ok(Ident::new_raw(name, span))
}

#[cfg(test)]
mod tests;
