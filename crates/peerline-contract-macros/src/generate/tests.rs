//! What the expansion *says*, as tokens.
//!
//! Reachable only from here: a doc comment leaves no runtime trace, so the
//! only place the manifest's prose can be checked to have arrived is against
//! the token stream, before it is handed to the compiler. The same is true of
//! a serde attribute — what it *does* is pinned by the wire tests in
//! `peerline-contract`, which expand a real manifest and compare literal
//! JSON; what it *is* can only be read here.

use super::*;

const FIXTURE: &str = r#"{
    "contract": "demo.thing", "version": 1,
    "description": "Why this contract exists.",
    "types": [
        { "form": "struct", "name": "Args", "description": "What a call carries.",
          "fields": [
            { "name": "id", "type": "string", "description": "Which one." },
            { "name": "extra", "type": "json", "optional": true,
              "description": "Anything else." } ] },
        { "form": "enum", "name": "Level", "description": "How bad.",
          "variants": [ { "name": "gone_away", "description": "It left." } ] },
        { "form": "union", "name": "Note", "tag": "kind", "description": "One item.",
          "variants": [ { "name": "line", "description": "Something was said.",
                          "fields": [ { "name": "text", "type": "string",
                                        "description": "What was said." } ] } ] }
    ],
    "errors": [ { "name": "demo", "code": 9, "data": "Args",
                  "description": "It did not work." } ],
    "ops": [
        { "name": "demo.thing.go", "kind": "unary", "args": "Args",
          "description": "Do the thing." },
        { "name": "demo.thing.watch", "kind": "stream", "args": "Args", "item": "Note",
          "errors": ["demo"], "description": "Watch the thing." }
    ]
}"#;

/// Expand a manifest with nothing said at the call site.
fn expand(json: &str) -> String {
    tokens(json, &[], &[])
        .expect("the fixture expands")
        .to_string()
}

/// The same, with the `runtime` clause said — the trait, the registration
/// and the client.
fn expand_with_runtime(json: &str) -> String {
    alongside(json, &[], &[], &[], true)
        .expect("the fixture expands")
        .to_string()
}

fn tokens(
    json: &str,
    imports: &[(&str, ImportTarget)],
    typescript: &[&str],
) -> syn::Result<TokenStream> {
    alongside(json, &[], imports, typescript, false)
}

/// `=> ::demo_vocab`, the mapping that names a module already holding the
/// vocabulary's expansion.
fn to(path: TokenStream) -> ImportTarget {
    ImportTarget::Path(path)
}

/// `=> inline`, the mapping that asks for copies here.
fn inline() -> ImportTarget {
    ImportTarget::Inline(Span::call_site())
}

/// Expand `json` with `others` sitting in the same directory, which is the
/// unit `contract!` loads: an imported shape is read out of the file that
/// declares it, so a fixture that imports needs that file present.
fn alongside(
    json: &str,
    others: &[&str],
    imports: &[(&str, ImportTarget)],
    typescript: &[&str],
    runtime: bool,
) -> syn::Result<TokenStream> {
    let mut all = vec![Manifest::parse(json).expect("the fixture is a valid manifest")];
    let wanted = all[0].id();
    for other in others {
        all.push(Manifest::parse(other).expect("the fixture is a valid manifest"));
    }
    let directory = Directory::new(all).expect("the fixtures agree with each other");
    let manifest = directory
        .manifests()
        .iter()
        .find(|m| m.id() == wanted)
        .expect("the one just put there");
    let imports: Vec<(LitStr, ImportTarget)> = imports
        .iter()
        .map(|(id, target)| (LitStr::new(id, Span::call_site()), target.clone()))
        .collect();
    let typescript: Vec<Ident> = typescript
        .iter()
        .map(|name| format_ident!("{}", name))
        .collect();
    let bindings = Bindings::resolve(manifest, &directory, &imports, &typescript, runtime)?;
    // No crate directory: an in-memory directory has no files, so there are no
    // dependency edges to anchor and nothing this argument could change.
    contract(manifest, &directory, &bindings, None, Span::call_site())
}

fn expansion() -> String {
    expand(FIXTURE)
}

fn rejection(json: &str, imports: &[(&str, ImportTarget)]) -> String {
    importing(json, imports, &[])
        .expect_err("this call site must be rejected")
        .to_string()
}

/// The reasoning moves. A generator that dropped the manifest's prose
/// would leave the contract's documentation in a file nobody reading the
/// Rust ever opens, which is the failure this whole format exists to fix.
#[test]
fn every_description_in_the_manifest_becomes_a_doc_comment() {
    let tokens = expansion();
    for expected in [
        "Why this contract exists.",
        "What a call carries.",
        "Which one.",
        "Anything else.",
        "How bad.",
        "It left.",
        "One item.",
        "Something was said.",
        "What was said.",
        "It did not work.",
        "Do the thing.",
        "Watch the thing.",
    ] {
        assert!(
            tokens.contains(&format!("doc = \" {expected}\"")),
            "`{expected}` did not reach the expansion",
        );
    }
}

/// The kind is in the docs as well as in the table, because the first
/// thing a caller needs to know about an op is which call to make.
#[test]
fn an_ops_docs_name_its_kind_and_its_shapes() {
    let tokens = expansion();
    assert!(tokens.contains("Kind: unary. Args: `Args`. Result: empty."));
    assert!(tokens.contains("Kind: stream. Args: `Args`. Items: `Note`."));
    assert!(tokens.contains("Raises: `ERR_DEMO`."));
}

/// Without this the compiled types outlive the files they came from — the
/// one failure a design whose claim is "they cannot drift" cannot afford.
///
/// One edge per file in the directory, and not just the manifest asked for:
/// every file was read, so an edit to any of them can change what this
/// expansion says or invalidate a check made while producing it.
///
/// A crate root with a `contracts` directory under it, as a call site's
/// `"contracts"` resolves to. Returns the root; the caller removes it.
///
/// The tracker tests are the only ones here that go through
/// [`Directory::load`], because the paths are the thing being asserted and an
/// in-memory directory has none.
fn crate_with_contracts(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "peerline-contract-macros-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("contracts");
    std::fs::create_dir_all(&dir).expect("a directory to load");
    std::fs::write(dir.join("demo.thing.v1.json"), IMPORTING).expect("written");
    std::fs::write(dir.join("demo.vocab.v1.json"), vocab("string")).expect("written");
    root
}

/// Expand `demo.thing/1` out of `<root>/contracts`, anchoring the dependency
/// edges at `crate_dir`.
fn tracked(root: &std::path::Path, crate_dir: Option<&std::path::Path>) -> String {
    tracked_as(root, crate_dir, to(quote!(::demo_vocab)))
}

/// The same, with the vocabulary mapped whichever way the caller wants.
fn tracked_as(
    root: &std::path::Path,
    crate_dir: Option<&std::path::Path>,
    target: ImportTarget,
) -> String {
    let directory = Directory::load(&root.join("contracts")).expect("the two manifests agree");
    let manifest = directory.get("demo.thing", 1).expect("just written");
    let imports = vec![(LitStr::new("demo.vocab/1", Span::call_site()), target)];
    let bindings = Bindings::resolve(manifest, &directory, &imports, &[], false).expect("mapped");
    contract(
        manifest,
        &directory,
        &bindings,
        crate_dir,
        Span::call_site(),
    )
    .expect("expands")
    .to_string()
}

#[test]
fn the_expansion_declares_every_manifest_in_the_directory_as_a_source_input() {
    let root = crate_with_contracts("edges");
    let tokens = tracked(&root, Some(&root));

    assert_eq!(tokens.matches("include_bytes").count(), 2, "{tokens}");
    assert!(tokens.contains("contracts/demo.thing.v1.json"), "{tokens}");
    assert!(
        tokens.contains("contracts/demo.vocab.v1.json"),
        "the vocabulary is an input too: {tokens}"
    );
    std::fs::remove_dir_all(&root).expect("cleaned up");
}

/// The expansion must be a function of the crate, not of where the crate
/// happens to be checked out.
///
/// An absolute literal here would make two machines with identical source
/// produce different token streams, which costs every shared build cache the
/// hits it should have had — and would make the expansion change when nothing
/// but the checkout path did. The macro still *resolves* an absolute path to
/// read the files; what it emits defers the join to the compiler that expands
/// the call site.
#[test]
fn a_dependency_edge_is_emitted_relative_to_the_invoking_crate() {
    let root = crate_with_contracts("portable");
    let tokens = tracked(&root, Some(&root));

    assert_eq!(
        tokens.matches("CARGO_MANIFEST_DIR").count(),
        2,
        "every edge is anchored at the invoking crate: {tokens}"
    );
    assert!(
        !tokens.contains(&root.to_string_lossy().to_string()),
        "this machine's checkout path must not appear in the expansion: {tokens}"
    );
    std::fs::remove_dir_all(&root).expect("cleaned up");
}

/// A directory with no crate to anchor it to keeps the absolute literal.
///
/// That is the call site that passed an absolute path: it has no
/// crate-relative spelling, and it was specific to this machine before the
/// macro ever saw it. Emitting a `..` chain out of the crate to reach it would
/// be a guess rather than a portability win.
#[test]
fn an_unanchorable_directory_keeps_its_absolute_path() {
    let root = crate_with_contracts("absolute");
    let tokens = tracked(&root, None);

    assert_eq!(tokens.matches("include_bytes").count(), 2, "{tokens}");
    assert!(
        !tokens.contains("CARGO_MANIFEST_DIR"),
        "there is nothing to anchor to: {tokens}"
    );
    assert!(
        tokens.contains(&root.join("contracts").to_string_lossy().to_string()),
        "{tokens}"
    );
    std::fs::remove_dir_all(&root).expect("cleaned up");
}

/// A file whose name disagrees with what it declares is one an implementor
/// who is not reading Rust cannot find from an import, which names a
/// contract and a version and never a path.
#[test]
fn a_misnamed_file_is_rejected_by_the_directory() {
    let dir =
        std::env::temp_dir().join(format!("peerline-contract-misnamed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory to load");
    std::fs::write(dir.join("vocab.json"), vocab("string")).expect("written");
    let err = Directory::load(&dir)
        .expect_err("the name disagrees")
        .to_string();
    assert!(err.contains("demo.vocab.v1.json"), "{err}");
    std::fs::remove_dir_all(&dir).expect("cleaned up");
}

/// Nothing the expansion emits is marked open, and that is the point.
///
/// This assertion used to run the other way: every generated enum and
/// union carried `#[non_exhaustive]`, on the reasoning that a contract
/// version is additive and a reader must tolerate a value it has not heard
/// of. The reasoning does not survive contact with the other half of the
/// same expansion. There is no catch-all variant, so serde rejects an
/// unknown value at the decode boundary — a Rust program cannot hold one
/// unless it was compiled against the newer manifest, and then it has the
/// variant. The attribute bought nothing on the wire and cost the
/// compile error: outside the crate that expanded the manifest it turns
/// "a new value is a build failure you must handle" into a wildcard arm
/// nobody revisits.
///
/// So the assertion is that the token never appears at all — for a
/// struct, for an enum, and for a union alike. If the format ever gains a
/// way for a *manifest* to declare a set open, this test changes together
/// with the emission of a catch-all variant, because the two have to
/// arrive together or the halves disagree again.
#[test]
fn nothing_generated_is_marked_open() {
    let tokens = expansion();
    assert_eq!(
        tokens.matches("non_exhaustive").count(),
        0,
        "`Level`, `Note` and `Args` are all closed sets, and a manifest is \
         the only thing that could say otherwise: {tokens}",
    );
}

/// An optional field is absent on the way out, not null.
#[test]
fn optional_fields_are_omitted_rather_than_nulled() {
    assert!(expansion().contains("skip_serializing_if = \"::core::option::Option::is_none\""));
}

/// A manifest is not a Rust file, and another target would take a name
/// Rust reserves without complaint — so Rust takes it too, as a raw
/// identifier, and the wire name is unchanged.
#[test]
fn a_field_named_after_a_rust_keyword_becomes_a_raw_identifier() {
    let json = FIXTURE.replace(r#""name": "id""#, r#""name": "type""#);
    assert!(expand(&json).contains("r#type"));
}

// --- `wire`: the identifier and the wire value are two things -------------

/// The whole of B1 in one assertion: the Rust name comes from `name` and the
/// serde rename comes from `wire`, so a value that is not a legal identifier
/// in anybody's language is still expressible.
#[test]
fn a_variants_wire_value_is_not_its_identifier() {
    let json = FIXTURE.replace(
        r#"{ "name": "gone_away", "description": "It left." }"#,
        r#"{ "name": "gone_away", "wire": "gone-away", "description": "It left." }"#,
    );
    let tokens = expand(&json);
    assert!(tokens.contains("rename = \"gone-away\""), "{tokens}");
    assert!(tokens.contains("GoneAway"), "{tokens}");
    assert!(!tokens.contains("rename = \"gone_away\""), "{tokens}");
}

/// Same for a union arm's discriminator, which is the same kind of string
/// and had the same constraint.
#[test]
fn a_union_arms_wire_value_is_not_its_identifier_either() {
    let json = FIXTURE.replace(
        r#"{ "name": "line", "description": "Something was said.","#,
        r#"{ "name": "line", "wire": "a line", "description": "Something was said.","#,
    );
    let tokens = expand(&json);
    assert!(tokens.contains("rename = \"a line\""), "{tokens}");
    assert!(tokens.contains("Line {"), "{tokens}");
}

// --- imports: the manifest names a contract, the call site names a module -

const IMPORTING: &str = r#"{
    "contract": "demo.thing", "version": 1,
    "description": "Why this contract exists.",
    "imports": [
        { "contract": "demo.vocab", "version": 1, "types": ["Shared"],
          "description": "The shape both contracts carry." }
    ],
    "types": [
        { "form": "struct", "name": "Args", "description": "What a call carries.",
          "fields": [ { "name": "shared", "type": "Shared",
                        "description": "The shared shape." } ] }
    ],
    "ops": [ { "name": "demo.thing.go", "kind": "unary", "args": "Args",
               "description": "Do the thing." } ]
}"#;

/// The vocabulary `IMPORTING` imports from, which sits in the same
/// directory. `{shape}` is substituted so that one fixture can declare
/// `Shared` at each rung of the comparison ladder in turn.
const VOCAB: &str = r#"{
    "contract": "demo.vocab", "version": 1, "role": "vocabulary",
    "description": "The shapes more than one contract carries.",
    "types": [
        { "form": "struct", "name": "Shared", "description": "The shared shape.",
          "fields": [ { "name": "value", "type": "{shape}",
                        "description": "Whatever it holds." } ] }
    ]
}"#;

fn vocab(shape: &str) -> String {
    VOCAB.replace("{shape}", shape)
}

/// Expand a manifest that imports `demo.vocab/1`, with the vocabulary in the
/// directory beside it.
fn importing(
    json: &str,
    imports: &[(&str, ImportTarget)],
    typescript: &[&str],
) -> syn::Result<TokenStream> {
    alongside(json, &[&vocab("string")], imports, typescript, false)
}

/// The manifest says `Shared`; the call site says where Rust keeps it. That
/// split is the point: the manifest stays readable by an implementor who has
/// never heard of a crate.
#[test]
fn an_imported_type_takes_the_path_the_call_site_gave_it() {
    let tokens = importing(
        IMPORTING,
        &[("demo.vocab/1", to(quote!(::demo_vocab)))],
        &[],
    )
    .expect("expands")
    .to_string();
    assert!(tokens.contains(":: demo_vocab :: Shared"), "{tokens}");
}

/// An unmapped import cannot be generated at all, and the error says what to
/// write rather than leaving a type that does not resolve — both spellings of
/// it, because there is no way for the macro to know which the call site
/// wanted and offering one would make the other hard to discover.
#[test]
fn an_import_the_call_site_did_not_map_is_a_compile_error() {
    let err = rejection(IMPORTING, &[]);
    assert!(err.contains("where its Rust types come from"), "{err}");
    assert!(err.contains(r#""demo.vocab/1" => ::some_crate"#), "{err}");
    assert!(err.contains(r#""demo.vocab/1" => inline"#), "{err}");
}

/// The other direction. A mapping for something the manifest does not import
/// does nothing, which is how a renamed vocabulary would go unnoticed.
#[test]
fn a_mapping_the_manifest_does_not_declare_is_rejected() {
    let err = rejection(
        IMPORTING,
        &[
            ("demo.vocab/1", to(quote!(::demo_vocab))),
            ("demo.other/1", to(quote!(::demo_other))),
        ],
    );
    assert!(err.contains("imports nothing of that name"), "{err}");
}

/// The rungs, end to end, and all of them read out of the *vocabulary*
/// rather than restated here. An imported shape holding passthrough JSON
/// drags its container down to `PartialEq`; one holding nothing but small
/// exact scalars lets the container be copied. Before the directory was the
/// unit, the call site had to say which — a fact sitting in a file two
/// directories away, where being wrong was silently over-conservative.
#[test]
fn an_imported_shapes_rung_is_read_from_the_manifest_that_declares_it() {
    let with = |shape: &str| {
        alongside(
            IMPORTING,
            &[&vocab(shape)],
            &[("demo.vocab/1", to(quote!(::demo_vocab)))],
            &[],
            false,
        )
        .expect("expands")
        .to_string()
    };

    let partial = with("json");
    assert!(partial.contains("PartialEq"), "{partial}");
    assert!(!partial.contains("cmp :: Eq"), "{partial}");

    let total = with("string");
    assert!(total.contains("cmp :: Eq"), "{total}");
    assert!(!total.contains("marker :: Copy"), "{total}");

    let copy = with("i53");
    assert!(copy.contains("marker :: Copy"), "{copy}");
}

// --- `=> inline`: the same shapes, generated here ---------------------------

/// The mode, in one assertion. `Shared` stops being a path into somebody
/// else's crate and becomes a `struct` in this expansion, with the
/// vocabulary's own prose on it — read out of `demo.vocab.v1.json`, which is
/// the only place that sentence exists.
#[test]
fn an_inlined_import_generates_the_vocabularys_shapes_here() {
    let tokens = importing(IMPORTING, &[("demo.vocab/1", inline())], &[])
        .expect("expands")
        .to_string();
    assert!(tokens.contains("pub struct Shared"), "{tokens}");
    assert!(
        !tokens.contains(":: demo_vocab"),
        "an inlined expansion names no other crate: {tokens}",
    );
    assert!(
        tokens.contains("doc = \" Whatever it holds.\""),
        "the copy carries the vocabulary's prose, not a second description: {tokens}",
    );
    // And the local shape that holds it reaches the copy by bare name, which
    // is what makes the rest of the generator unable to tell the two apart.
    assert!(tokens.contains("pub shared : Shared"), "{tokens}");
}

/// A copy is generated the way a local shape is, not in some reduced form:
/// the serde attributes are what the wire test at the other end depends on,
/// and the constructor and the rung are what make it usable at all.
#[test]
fn a_copy_is_generated_exactly_as_a_local_shape_is() {
    let tokens = alongside(
        IMPORTING,
        &[&vocab("i53")],
        &[("demo.vocab/1", inline())],
        &[],
        false,
    )
    .expect("expands")
    .to_string();
    // The rung is read off the *vocabulary*, as it always was — an exact
    // fixed-size scalar, so the copy is copyable.
    assert!(tokens.contains("marker :: Copy"), "{tokens}");
    assert!(
        tokens.contains("impl Shared { # [doc = \" A `Shared`, given everything it requires.\"]"),
        "{tokens}",
    );
    assert!(tokens.contains("serde :: Serialize"), "{tokens}");
}

/// The clause answers per vocabulary, so one call site may do both. That is
/// the case worth pinning: a contract importing something it wants to hand
/// onward in Rust and something it only decodes.
#[test]
fn one_clause_may_inline_one_import_and_map_another_to_a_crate() {
    const TWO: &str = r#"{
        "contract": "demo.thing", "version": 1,
        "description": "Why this contract exists.",
        "imports": [
            { "contract": "demo.vocab", "version": 1, "types": ["Shared"],
              "description": "The shape both contracts carry." },
            { "contract": "demo.other", "version": 1, "types": ["Elsewhere"],
              "description": "The shape that stays where it is." }
        ],
        "types": [
            { "form": "struct", "name": "Args", "description": "What a call carries.",
              "fields": [
                { "name": "shared", "type": "Shared", "description": "The shared shape." },
                { "name": "far", "type": "Elsewhere", "description": "The other one." } ] }
        ],
        "ops": [ { "name": "demo.thing.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." } ]
    }"#;
    const OTHER: &str = r#"{
        "contract": "demo.other", "version": 1, "role": "vocabulary",
        "description": "A vocabulary somebody else's crate expands.",
        "types": [
            { "form": "struct", "name": "Elsewhere", "description": "It stays put.",
              "fields": [ { "name": "id", "type": "string",
                            "description": "Which one." } ] }
        ]
    }"#;
    let tokens = alongside(
        TWO,
        &[&vocab("string"), OTHER],
        &[
            ("demo.vocab/1", inline()),
            ("demo.other/1", to(quote!(::demo_other))),
        ],
        &[],
        false,
    )
    .expect("expands")
    .to_string();
    assert!(tokens.contains("pub struct Shared"), "{tokens}");
    assert!(
        tokens.contains("pub far : :: demo_other :: Elsewhere"),
        "{tokens}"
    );
    assert!(
        !tokens.contains("pub struct Elsewhere"),
        "the path mapping is untouched by the inline one: {tokens}",
    );
}

/// An import lists the shapes this contract's *wire* mentions, which is
/// contract documentation and not a build-order declaration. A path mapping
/// never needed more, because the other crate had already compiled whatever
/// those shapes hold; a copy is a `struct` written out here, so every shape
/// its fields name has to be a type in this scope too.
#[test]
fn inlining_brings_the_shapes_the_listed_ones_hold() {
    const NESTED: &str = r#"{
        "contract": "demo.vocab", "version": 1, "role": "vocabulary",
        "description": "The shapes more than one contract carries.",
        "types": [
            { "form": "struct", "name": "Shared", "description": "The shared shape.",
              "fields": [ { "name": "held", "type": "Held",
                            "description": "What it holds." } ] },
            { "form": "enum", "name": "Held", "description": "Which sort.",
              "variants": [ { "name": "one", "description": "The only one." } ] }
        ]
    }"#;
    let tokens = alongside(
        IMPORTING,
        &[NESTED],
        &[("demo.vocab/1", inline())],
        &[],
        false,
    )
    .expect("expands")
    .to_string();
    assert!(tokens.contains("pub struct Shared"), "{tokens}");
    assert!(
        tokens.contains("pub enum Held"),
        "`Held` is not listed in the import and is still needed: {tokens}",
    );
}

/// The collision the manifest format cannot see. A name the import *lists*
/// can never clash with a local one — the validator refuses a manifest that
/// declares and imports one name — but nothing checks the shapes a copy drags
/// in behind it, and those are exactly the ones nobody wrote down.
#[test]
fn a_copy_colliding_with_a_local_shape_names_both_sides() {
    const CLASHING: &str = r#"{
        "contract": "demo.vocab", "version": 1, "role": "vocabulary",
        "description": "The shapes more than one contract carries.",
        "types": [
            { "form": "struct", "name": "Shared", "description": "The shared shape.",
              "fields": [ { "name": "held", "type": "Args",
                            "description": "What it holds." } ] },
            { "form": "struct", "name": "Args", "description": "A name the importer also uses.",
              "fields": [ { "name": "id", "type": "string",
                            "description": "Which one." } ] }
        ]
    }"#;
    let err = alongside(
        IMPORTING,
        &[CLASHING],
        &[("demo.vocab/1", inline())],
        &[],
        false,
    )
    .expect_err("`Args` is declared twice over")
    .to_string();
    assert!(
        err.contains("the `inline` import of `demo.vocab/1`"),
        "{err}"
    );
    assert!(err.contains("`Args`"), "{err}");
    assert!(
        err.contains("this manifest already claims that name"),
        "{err}"
    );
    assert!(
        err.contains("demo.vocab/1` to a crate rather than `inline`"),
        "{err}"
    );
}

/// The same rule against a name a *path* mapping already supplies. The two
/// would otherwise be one identifier meaning two types, with whichever the
/// generator reached first winning silently.
#[test]
fn a_copy_colliding_with_a_shape_another_import_supplies_is_rejected() {
    const TWO: &str = r#"{
        "contract": "demo.thing", "version": 1,
        "description": "Why this contract exists.",
        "imports": [
            { "contract": "demo.vocab", "version": 1, "types": ["Shared"],
              "description": "The shape both contracts carry." },
            { "contract": "demo.other", "version": 1, "types": ["Held"],
              "description": "The shape that stays where it is." }
        ],
        "types": [
            { "form": "struct", "name": "Args", "description": "What a call carries.",
              "fields": [
                { "name": "shared", "type": "Shared", "description": "The shared shape." },
                { "name": "far", "type": "Held", "description": "The other one." } ] }
        ],
        "ops": [ { "name": "demo.thing.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." } ]
    }"#;
    const HOLDER: &str = r#"{
        "contract": "demo.vocab", "version": 1, "role": "vocabulary",
        "description": "The shapes more than one contract carries.",
        "types": [
            { "form": "struct", "name": "Shared", "description": "The shared shape.",
              "fields": [ { "name": "held", "type": "Held",
                            "description": "What it holds." } ] },
            { "form": "enum", "name": "Held", "description": "Which sort.",
              "variants": [ { "name": "one", "description": "The only one." } ] }
        ]
    }"#;
    const OTHER: &str = r#"{
        "contract": "demo.other", "version": 1, "role": "vocabulary",
        "description": "A different vocabulary that happens to use the name.",
        "types": [
            { "form": "enum", "name": "Held", "description": "Which sort, elsewhere.",
              "variants": [ { "name": "two", "description": "The other one." } ] }
        ]
    }"#;
    let err = alongside(
        TWO,
        &[HOLDER, OTHER],
        &[
            ("demo.vocab/1", inline()),
            ("demo.other/1", to(quote!(::demo_other))),
        ],
        &[],
        false,
    )
    .expect_err("`Held` would be two types under one name")
    .to_string();
    assert!(
        err.contains("the import of `demo.other/1` already claims"),
        "{err}"
    );
    assert!(err.contains("`Held`"), "{err}");
}

/// A copy occupies the generated namespace exactly as a local shape does, so
/// a vocabulary holding a `…Client` collides with the `runtime` clause in
/// precisely the same way — and the error has to say which file to look in,
/// because it is not the one the call site names.
#[test]
fn a_copy_named_like_a_generated_name_is_rejected_too() {
    const CLIENTISH: &str = r#"{
        "contract": "demo.vocab", "version": 1, "role": "vocabulary",
        "description": "The shapes more than one contract carries.",
        "types": [
            { "form": "struct", "name": "DemoThingClient", "description": "An ordinary word.",
              "fields": [ { "name": "id", "type": "string",
                            "description": "Which one." } ] }
        ]
    }"#;
    const IMPORTS_IT: &str = r#"{
        "contract": "demo.thing", "version": 1,
        "description": "Why this contract exists.",
        "imports": [
            { "contract": "demo.vocab", "version": 1, "types": ["DemoThingClient"],
              "description": "The shape both contracts carry." }
        ],
        "types": [
            { "form": "struct", "name": "Args", "description": "What a call carries.",
              "fields": [ { "name": "who", "type": "DemoThingClient",
                            "description": "The shared shape." } ] }
        ],
        "ops": [ { "name": "demo.thing.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." } ]
    }"#;
    let err = alongside(
        IMPORTS_IT,
        &[CLIENTISH],
        &[("demo.vocab/1", inline())],
        &[],
        true,
    )
    .expect_err("the name is taken")
    .to_string();
    assert!(
        err.contains("the `inline` import of `demo.vocab/1`"),
        "{err}"
    );
    assert!(err.contains("already declares a shape"), "{err}");
    // And the same manifest mapped to a path is fine, because then the name
    // is `::demo_vocab::DemoThingClient` and never a bare one.
    alongside(
        IMPORTS_IT,
        &[CLIENTISH],
        &[("demo.vocab/1", to(quote!(::demo_vocab)))],
        &[],
        true,
    )
    .expect("a path mapping claims no local name");
}

/// The point of the `runtime` interplay: a trait method and a client method
/// take the *copy*, not a path into a crate this expansion does not name.
/// Anything else would be a signature that cannot compile.
#[test]
fn runtime_signatures_name_the_copy_when_the_import_is_inlined() {
    const STREAMING: &str = r#"{
        "contract": "demo.thing", "version": 1,
        "description": "Why this contract exists.",
        "imports": [
            { "contract": "demo.vocab", "version": 1, "types": ["Shared"],
              "description": "The shape both contracts carry." }
        ],
        "types": [
            { "form": "struct", "name": "Args", "description": "What a call carries.",
              "fields": [ { "name": "id", "type": "string",
                            "description": "Which one." } ] }
        ],
        "ops": [
            { "name": "demo.thing.go", "kind": "unary", "args": "Args", "result": "Shared",
              "description": "Do the thing." },
            { "name": "demo.thing.watch", "kind": "stream", "args": "Args", "item": "Shared",
              "description": "Watch the thing." }
        ]
    }"#;
    let tokens = alongside(
        STREAMING,
        &[&vocab("string")],
        &[("demo.vocab/1", inline())],
        &[],
        true,
    )
    .expect("expands")
    .to_string();
    assert!(
        tokens.contains(":: core :: result :: Result < Shared , :: peerline :: wire :: RpcError >"),
        "the trait answers the copy: {tokens}",
    );
    assert!(
        tokens.contains("pub fn send (& self , item : & Shared)"),
        "the item sink carries the copy: {tokens}",
    );
    assert!(
        tokens.contains("StreamReceiver < Shared >"),
        "and so does the client: {tokens}",
    );
    assert!(
        !tokens.contains(":: demo_vocab"),
        "nothing in the stubs names another crate: {tokens}",
    );
}

/// An inlined shape is this expansion's, so its TypeScript is this call
/// site's to ask for — the clause's one prohibition is against exporting a
/// type that belongs to another crate's expansion, and this is not one.
#[test]
fn the_typescript_clause_may_name_an_inlined_shape() {
    let tokens = importing(
        IMPORTING,
        &[("demo.vocab/1", inline())],
        &["Shared", "Args"],
    )
    .expect("expands")
    .to_string();
    assert_eq!(
        tokens.matches("derive (:: ts_rs :: TS)").count(),
        2,
        "both shapes carry the derive: {tokens}",
    );
}

/// And it is held to the same member rule. `ts_rs` derives through members
/// whether the member was written here or copied here, so leaving the copy
/// out of the clause fails inside `ts_rs` unless the macro says it first.
#[test]
fn an_inlined_member_left_out_of_the_typescript_clause_is_named() {
    let err = importing(IMPORTING, &[("demo.vocab/1", inline())], &["Args"])
        .expect_err("`Args` holds a `Shared`")
        .to_string();
    assert!(err.contains("add `Shared` to the clause"), "{err}");
}

/// The rebuild edges are directory-wide and were before this feature, which
/// is what makes inlining safe: the copies are generated out of a file this
/// crate does not name and would otherwise have no reason to watch. Asserted
/// against the inline mapping specifically, because "it already worked" is
/// exactly the kind of claim that stops being true quietly.
#[test]
fn an_inlined_vocabularys_file_is_a_source_input_of_this_crate() {
    let root = crate_with_contracts("inlined-edges");
    let tokens = tracked_as(&root, Some(&root), inline());

    assert!(tokens.contains("pub struct Shared"), "{tokens}");
    assert_eq!(tokens.matches("include_bytes").count(), 2, "{tokens}");
    assert!(
        tokens.contains("contracts/demo.vocab.v1.json"),
        "editing the vocabulary must rebuild the crate that copied it: {tokens}",
    );
    std::fs::remove_dir_all(&root).expect("cleaned up");
}

/// A vocabulary has no producer, so the two lists a producer registers from
/// would be empty declarations nobody could use.
#[test]
fn a_vocabulary_expands_to_shapes_and_no_op_tables() {
    const VOCABULARY: &str = r#"{
        "contract": "demo.vocab", "version": 1, "role": "vocabulary",
        "description": "Shapes several contracts carry.",
        "types": [ { "form": "struct", "name": "Shared", "description": "The shape.",
                     "fields": [ { "name": "id", "type": "string",
                                   "description": "Which one." } ] } ]
    }"#;
    let tokens = expand(VOCABULARY);
    assert!(tokens.contains("pub struct Shared"), "{tokens}");
    assert!(!tokens.contains("OPS"), "{tokens}");
    assert!(!tokens.contains("OP_KINDS"), "{tokens}");
}

// --- the three field states, and defaults ---------------------------------

/// A nullable field writes its key every time. The absence of
/// `skip_serializing_if` is the whole mechanism, so it is what is asserted.
#[test]
fn a_nullable_field_keeps_its_key_and_an_optional_one_does_not() {
    let json = FIXTURE.replace(
        r#""optional": true,
              "description": "Anything else."#,
        r#""nullable": true,
              "description": "Anything else."#,
    );
    let tokens = expand(&json);
    assert!(!tokens.contains("skip_serializing_if"), "{tokens}");
    assert!(tokens.contains("# [serde (default)]"), "{tokens}");
}

/// A default is a value, not an absence: the generated function is what
/// serde calls, and `Default` is written out rather than derived because
/// `bool::default()` is not what the manifest said.
#[test]
fn a_default_becomes_a_function_serde_calls_and_a_default_impl() {
    const DEFAULTED: &str = r#"{
        "contract": "demo.thing", "version": 1,
        "description": "Why this contract exists.",
        "types": [
            { "form": "struct", "name": "Args", "description": "What a call carries.",
              "fields": [ { "name": "include_history", "type": "bool", "default": true,
                            "description": "Whether to include it." } ] }
        ],
        "ops": [ { "name": "demo.thing.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." } ]
    }"#;
    let tokens = expand(DEFAULTED);
    assert!(
        tokens.contains("serde (default = \"Args::default_include_history\")"),
        "{tokens}",
    );
    assert!(
        tokens.contains("pub fn default_include_history () -> bool { true }"),
        "{tokens}",
    );
    assert!(
        tokens.contains("impl :: core :: default :: Default for Args"),
        "{tokens}"
    );
    // Nothing is required, so there is no constructor to duplicate it.
    assert!(!tokens.contains("pub fn new"), "{tokens}");
}

/// A flattened field has no key of its own; its type's keys are the ones on
/// the wire.
#[test]
fn a_flattened_field_writes_its_types_keys_beside_its_siblings() {
    let json = IMPORTING.replace(
        r#"{ "name": "shared", "type": "Shared","#,
        r#"{ "name": "shared", "type": "Shared", "flatten": true,"#,
    );
    let tokens = importing(&json, &[("demo.vocab/1", to(quote!(::demo_vocab)))], &[])
        .expect("expands")
        .to_string();
    assert!(tokens.contains("# [serde (flatten)]"), "{tokens}");
}

// --- unions: two taggings, and an arm that carries one whole shape --------

const KIND_FILTER: &str = r#"{
    "contract": "demo.thing", "version": 1,
    "description": "Why this contract exists.",
    "types": [
        { "form": "struct", "name": "Args", "description": "What a call carries.",
          "fields": [ { "name": "kinds", "type": "KindFilter",
                        "description": "Which rows." } ] },
        { "form": "union", "name": "KindFilter", "tagging": "external",
          "description": "Which kinds pass.",
          "variants": [
            { "name": "only", "payload": { "array": "string" },
              "description": "Keep only these." },
            { "name": "except", "payload": { "array": "string" },
              "description": "Drop these." } ] }
    ],
    "ops": [ { "name": "demo.thing.go", "kind": "unary", "args": "Args",
               "description": "Do the thing." } ]
}"#;

/// An externally tagged union has no discriminator field, so the expansion
/// must not carry serde's internal-tag attribute — which is the difference
/// between `{"only":[…]}` and `{"kind":"only",…}`.
#[test]
fn an_external_union_carries_no_tag_attribute() {
    let tokens = expand(KIND_FILTER);
    assert!(!tokens.contains("serde (tag"), "{tokens}");
    assert!(
        tokens.contains("Only (:: std :: vec :: Vec < :: std :: string :: String >)"),
        "{tokens}",
    );
}

/// A payload variant keeps the shape whole, and the accessor is what makes
/// that worth having: a consumer takes the shape back out instead of
/// rebuilding it from fields.
#[test]
fn a_payload_variant_is_a_newtype_with_an_accessor() {
    let json = FIXTURE.replace(
        r#"{ "name": "line", "description": "Something was said.",
                          "fields": [ { "name": "text", "type": "string",
                                        "description": "What was said." } ] }"#,
        r#"{ "name": "line", "payload": "Args",
                          "description": "Something was said." }"#,
    );
    let tokens = expand(&json);
    assert!(tokens.contains("Line (Args)"), "{tokens}");
    assert!(
        tokens.contains("pub fn as_line (& self) -> :: core :: option :: Option < & Args >"),
        "{tokens}",
    );
}

/// An op that sometimes answers nothing says so in its result, and the doc
/// line a caller reads is where that shows up on this target.
#[test]
fn a_nullable_result_reads_as_an_option() {
    let json = FIXTURE.replace(
        r#""args": "Args",
          "description": "Do the thing."#,
        r#""args": "Args", "result": { "nullable": "i64" },
          "description": "Do the thing."#,
    );
    let tokens = expand(&json);
    assert!(tokens.contains("Result: `Option<i64>`."), "{tokens}");
}

// --- the safe-integer scalars, and the clause that reads them ------------

/// A manifest carrying every shape the `typescript` clause has an opinion
/// about: a safe integer, a wide one, passthrough JSON, an absent-able
/// field, and a closed set whose wire spelling is not its name.
const BROWSER: &str = r#"{
    "contract": "demo.thing", "version": 1,
    "description": "Why this contract exists.",
    "types": [
        { "form": "struct", "name": "Row", "description": "One row.",
          "fields": [
            { "name": "at_ms", "type": "i53", "description": "When." },
            { "name": "size", "type": "u53", "description": "How much." },
            { "name": "opaque_id", "type": "u64", "description": "Somebody else's id." },
            { "name": "config", "type": "json", "description": "Uninterpreted." },
            { "name": "kind", "type": "Kind", "description": "Which sort." },
            { "name": "since_ms", "type": "i53", "optional": true,
              "description": "From when." },
            { "name": "until_ms", "type": "i53", "nullable": true,
              "description": "Until when." } ] },
        { "form": "enum", "name": "Kind", "description": "Which sort.",
          "variants": [ { "name": "full_message", "wire": "full-message",
                          "description": "All of it." } ] }
    ],
    "ops": [ { "name": "demo.thing.go", "kind": "unary", "args": "Row",
               "description": "Do the thing." } ]
}"#;

fn browser_tokens(names: &[&str]) -> String {
    tokens(BROWSER, &[], names).expect("expands").to_string()
}

/// The whole point of the keyword: in Rust it is the same integer it always
/// was, so nothing about equality, hashing or a database column moves.
#[test]
fn a_safe_integer_generates_the_same_rust_the_wide_one_does() {
    let tokens = expand(BROWSER);
    assert!(tokens.contains("pub at_ms : i64"), "{tokens}");
    assert!(tokens.contains("pub size : u64"), "{tokens}");
    assert!(tokens.contains("pub opaque_id : u64"), "{tokens}");
}

/// A shape of exact fixed-size scalars is copyable, and a safe integer is
/// one — the same rung an `i64` reaches, read off the manifest.
#[test]
fn a_safe_integer_is_an_exact_fixed_size_scalar() {
    let json = r#"{
        "contract": "demo.thing", "version": 1,
        "description": "Why this contract exists.",
        "types": [
            { "form": "struct", "name": "Stamp", "description": "When.",
              "fields": [ { "name": "at_ms", "type": "i53", "description": "When." } ] }
        ],
        "ops": [ { "name": "demo.thing.go", "kind": "unary", "args": "Stamp",
                   "description": "Do the thing." } ]
    }"#;
    let tokens = expand(json);
    assert!(tokens.contains("marker :: Copy"), "{tokens}");
    assert!(tokens.contains("cmp :: Eq"), "{tokens}");
}

/// Without the clause there is no mention of `ts_rs` anywhere in the
/// expansion — which is what lets a crate with no business linking a
/// TypeScript generator expand the same manifest.
#[test]
fn a_call_site_that_does_not_ask_emits_no_typescript_at_all() {
    let tokens = expand(BROWSER);
    assert!(!tokens.contains("ts_rs"), "{tokens}");
    assert!(!tokens.contains("# [ts ("), "{tokens}");
}

/// The derive, and the three mappings `ts_rs` could not make for itself:
/// a safe integer is a `number` where the wide one is left as the `bigint`
/// it honestly is, passthrough JSON is `unknown`, and an absent-able key is
/// `key?: T`.
#[test]
fn the_clause_emits_the_derive_and_the_mappings_ts_rs_cannot_infer() {
    let tokens = browser_tokens(&["Row", "Kind"]);
    assert!(tokens.contains("derive (:: ts_rs :: TS)"), "{tokens}");
    assert_eq!(
        tokens.matches(r#"# [ts (type = "number")]"#).count(),
        2,
        "the two safe integers, and not the `u64`: {tokens}",
    );
    assert!(tokens.contains(r#"# [ts (type = "unknown")]"#), "{tokens}");
    assert!(
        tokens.contains(r#"# [ts (optional , type = "number")]"#),
        "{tokens}",
    );
    assert!(
        tokens.contains(r#"# [ts (type = "number | null")]"#),
        "a nullable key is written, and may be null: {tokens}",
    );
}

/// The wire spelling of a value reaches `ts_rs` in its own attribute rather
/// than through its optional serde compatibility, because which Cargo
/// features a downstream crate turns on is not something a contract may
/// depend on.
#[test]
fn a_variants_wire_spelling_does_not_depend_on_a_cargo_feature() {
    let tokens = browser_tokens(&["Row", "Kind"]);
    assert!(
        tokens.contains(r#"# [ts (rename = "full-message")]"#),
        "{tokens}"
    );
}

/// `ts_rs` derives through a shape's members, so a member left out of the
/// clause fails to compile somewhere that does not mention the clause. The
/// macro says it instead, naming what to add.
#[test]
fn a_member_left_out_of_the_clause_is_named_rather_than_left_to_ts_rs() {
    let err = tokens(BROWSER, &[], &["Row"])
        .expect_err("Row holds a Kind")
        .to_string();
    assert!(err.contains("add `Kind` to the clause"), "{err}");
}

#[test]
fn the_clause_may_only_name_shapes_the_manifest_declares() {
    let err = tokens(BROWSER, &[], &["Nonesuch"])
        .expect_err("there is no such shape")
        .to_string();
    assert!(err.contains("not a shape this manifest declares"), "{err}");

    let err = tokens(BROWSER, &[], &["Kind", "Kind"])
        .expect_err("named twice")
        .to_string();
    assert!(err.contains("named twice"), "{err}");
}

/// An imported shape's Rust type belongs to the expansion of the manifest it
/// came from, so its TypeScript is asked for there.
#[test]
fn the_clause_may_not_name_a_shape_imported_to_a_path() {
    let err = importing(
        IMPORTING,
        &[("demo.vocab/1", to(quote!(::demo_vocab)))],
        &["Shared"],
    )
    .expect_err("`Shared` is another expansion's type")
    .to_string();
    assert!(err.contains("imported from another manifest"), "{err}");
}

// --- `runtime`: the dispatch, generated from the same table as the kinds ---

/// The clause is opt-in because the paths it emits are not this crate's to
/// promise. Every invocation written before it existed must go on compiling
/// in a crate that has never heard of `peerline`, which means the expansion
/// may not so much as name it.
///
/// `::peerline_contract` is a different matter and is deliberately not
/// covered here: `OP_KINDS` has always been typed by it, so it is a
/// requirement the call site already met.
#[test]
fn nothing_names_the_peer_runtime_unless_the_clause_asks_for_it() {
    let tokens = expansion();
    assert!(
        !tokens.contains(":: peerline ::"),
        "a call site without the clause must link no peer runtime: {tokens}",
    );
}

/// The four names the clause adds, for a contract called `demo.thing`.
#[test]
fn the_clause_generates_the_trait_the_registration_the_client_and_a_sink() {
    let tokens = expand_with_runtime(FIXTURE);
    assert!(tokens.contains("pub trait DemoThingService"), "{tokens}");
    assert!(
        tokens.contains("pub fn register_demo_thing_handlers"),
        "{tokens}",
    );
    assert!(tokens.contains("pub struct DemoThingClient"), "{tokens}");
    assert!(tokens.contains("pub struct WatchItems"), "{tokens}");
}

/// **The assertion the whole clause exists for.** `go` is unary and `watch`
/// is a stream, and the registration has to reach for a different peerline
/// call for each. A generator that emitted `on_request` for both would
/// produce a producer that answers a `call_stream` with a kind mismatch at
/// run time — the exact failure `OP_KINDS` was added to make checkable, here
/// made unwritable instead.
#[test]
fn each_op_is_registered_with_the_call_its_declared_kind_names() {
    let tokens = expand_with_runtime(FIXTURE);
    assert!(tokens.contains("on_request (OP_GO"), "{tokens}");
    assert!(tokens.contains("on_stream_request (OP_WATCH"), "{tokens}");
    // `on_stream_request (OP_WATCH` also matches `on_request (OP_WATCH` as a
    // substring, so the negative has to be spelled with what precedes it.
    assert!(
        !tokens.contains(". on_request (OP_WATCH"),
        "a stream op registered as unary is the failure this is for: {tokens}",
    );
    assert!(
        !tokens.contains("on_stream_request (OP_GO"),
        "and the mistake runs both ways: {tokens}",
    );
}

/// The client is the mirror image: one method per op, with that op's kind
/// baked in, so a consumer cannot pass a stream op to `call`.
#[test]
fn the_client_calls_each_op_the_way_its_kind_requires() {
    let tokens = expand_with_runtime(FIXTURE);
    assert!(tokens.contains("self . peer . call (OP_GO"), "{tokens}");
    assert!(
        tokens.contains("self . peer . call_stream (OP_WATCH"),
        "{tokens}",
    );
}

/// A bare `StreamSender` takes anything serialisable, so a producer that put
/// the wrong shape on a stream would be found by whatever decoded it, at the
/// consumer, at run time. The sink carries the manifest's item type, which
/// moves that to the compiler.
#[test]
fn the_stream_item_type_is_on_the_sink_rather_than_left_to_the_sender() {
    let tokens = expand_with_runtime(FIXTURE);
    assert!(
        tokens.contains("pub fn send (& self , item : & Note)"),
        "{tokens}"
    );
}

/// An op whose manifest declares no `result` answers nothing at all, every
/// time — which is `()`. Not an `Option`: a result that is *sometimes* there
/// is a nullable schema, and the format already distinguishes the two.
#[test]
fn an_op_with_no_result_returns_nothing_rather_than_an_option() {
    let tokens = expand_with_runtime(FIXTURE);
    assert!(
        tokens.contains(":: core :: result :: Result < () , :: peerline :: wire :: RpcError >"),
        "{tokens}",
    );
}

/// peerline refuses params that are not a JSON object, so an op with no
/// arguments still travels as one. A map is what both ends of that spell —
/// `()` would insist on `null` and be refused before the handler ran.
#[test]
fn an_op_with_no_arguments_still_travels_as_an_object() {
    let json = FIXTURE.replace(r#""kind": "unary", "args": "Args","#, r#""kind": "unary","#);
    let tokens = expand_with_runtime(&json);
    assert!(
        tokens.contains(":: serde_json :: Map :: new ()"),
        "{tokens}"
    );
}

/// `…Client` and `…Items` are ordinary words, and a manifest is free to have
/// declared one. Left alone the collision lands as a duplicate definition on
/// a generated line that mentions neither the manifest nor the clause.
#[test]
fn a_shape_named_like_a_generated_name_is_rejected_at_the_call_site() {
    let json = FIXTURE.replace(r#""name": "Level""#, r#""name": "DemoThingClient""#);
    let err = alongside(&json, &[], &[], &[], true)
        .expect_err("the name is taken")
        .to_string();
    assert!(err.contains("already declares a shape"), "{err}");
}

/// One registration per op, beside the one for all of them. Without these a
/// producer that wants a different surface per transport — the whole contract
/// on a socket the kernel attests, reads only on a port — cannot use the
/// generated wiring at all, and goes back to hand-writing the very
/// `on_request` calls the clause exists to remove.
#[test]
fn every_op_gets_a_registration_of_its_own() {
    let tokens = expand_with_runtime(FIXTURE);
    assert!(tokens.contains("pub fn register_go"), "{tokens}");
    assert!(tokens.contains("pub fn register_watch"), "{tokens}");
}

/// A per-op registration is the *same* wiring, not a second one: the
/// whole-contract function calls it. Two copies of the wiring would be two
/// places for a kind to be wrong, which is the failure this whole clause was
/// written to make unwritable.
#[test]
fn the_whole_contract_registration_is_written_in_terms_of_the_per_op_ones() {
    let tokens = expand_with_runtime(FIXTURE);
    assert!(
        tokens.contains("register_go (peer , :: std :: sync :: Arc :: clone (& imp))"),
        "{tokens}",
    );
    assert!(
        tokens.contains("register_watch (peer , :: std :: sync :: Arc :: clone (& imp))"),
        "{tokens}",
    );
    // And it is only the delegation: a wiring left behind beside the calls
    // would be a second registration of the same op on the same peer.
    assert_eq!(
        tokens.matches("on_request (OP_GO").count(),
        1,
        "`go` is wired exactly once: {tokens}",
    );
    assert_eq!(
        tokens.matches("on_stream_request (OP_WATCH").count(),
        1,
        "`watch` is wired exactly once: {tokens}",
    );
}

/// Each per-op function carries its own op's kind, because it *is* the
/// generated wiring rather than a wrapper around a name. A subset registered
/// one op at a time is as kind-correct as the whole contract registered at
/// once — otherwise the subset path would buy back the mistake.
#[test]
fn a_per_op_registration_keeps_the_kind_the_manifest_declares() {
    let tokens = expand_with_runtime(FIXTURE);
    let go = body_of(&tokens, "pub fn register_go");
    assert!(go.contains("on_request (OP_GO"), "{go}");
    assert!(!go.contains("on_stream_request"), "{go}");
    let watch = body_of(&tokens, "pub fn register_watch");
    assert!(watch.contains("on_stream_request (OP_WATCH"), "{watch}");
    assert!(!watch.contains(". on_request (OP_WATCH"), "{watch}");
}

/// Everything the two entry points promise is in the prose, because the
/// reason to register one op rather than all of them is a security argument
/// and not a preference. An adopter who reads only the signature would take
/// the per-op functions for a convenience.
#[test]
fn the_subset_use_case_is_documented_on_both_registrations() {
    let tokens = expand_with_runtime(FIXTURE);
    assert!(
        tokens
            .matches("registering none of an op leaves it `MethodNotFound`, which is the point")
            .count()
            >= 3,
        "the point of a subset belongs on every registration: {tokens}",
    );
}

/// A generated function name is as collidable as a generated type name, and
/// the one reachable collision is an op that spells the whole-contract
/// registration. Left alone it is a duplicate definition on a generated line
/// that mentions neither the manifest nor the clause.
#[test]
fn an_op_that_spells_the_whole_contract_registration_is_rejected() {
    let json = FIXTURE.replace(
        r#""name": "demo.thing.go""#,
        r#""name": "demo.thing.demo_thing_handlers""#,
    );
    let err = alongside(&json, &[], &[], &[], true)
        .expect_err("the name is taken")
        .to_string();
    assert!(err.contains("register_demo_thing_handlers"), "{err}");
    assert!(err.contains("the whole-contract registration"), "{err}");
}

/// The token text between `marker` and the next item, which is how a test
/// reads one generated function rather than the whole expansion. Functions
/// are emitted one after another, so the next `pub fn` or `pub trait` is the
/// end of this one.
fn body_of(tokens: &str, marker: &str) -> String {
    let start = tokens
        .find(marker)
        .unwrap_or_else(|| panic!("`{marker}` is not in the expansion: {tokens}"))
        + marker.len();
    let rest = &tokens[start..];
    let end = ["pub fn ", "pub trait ", "pub struct "]
        .iter()
        .filter_map(|next| rest.find(next))
        .min()
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// A vocabulary has no ops, so there is no producer to implement and no
/// client to call. Asking for the stubs there is a mistake about which
/// manifest is being expanded, and generating nothing would leave it to be
/// found as a missing trait somewhere else.
#[test]
fn the_clause_is_rejected_on_a_vocabulary() {
    let err = alongside(&vocab("string"), &[], &[], &[], true)
        .expect_err("a vocabulary has no producer")
        .to_string();
    assert!(err.contains("is a vocabulary"), "{err}");
}

// --- `produced()`: the declaration nobody should write twice --------------

/// Unconditional, unlike the stubs: it names `::peerline_contract`, which
/// `OP_KINDS` already required of every call site. So it costs nobody a
/// dependency, and a producer stops restating a name, a version and an op
/// list it could restate wrongly.
#[test]
fn produced_is_generated_without_any_clause_at_all() {
    let tokens = expansion();
    assert!(
        tokens.contains(
            ":: peerline_contract :: Produced :: with_ops (CONTRACT , VERSION , OPS . iter () . copied ())"
        ),
        "{tokens}",
    );
}

/// Nobody produces a vocabulary, so there is nothing for it to declare.
#[test]
fn a_vocabulary_declares_nothing_produced() {
    let tokens = alongside(&vocab("string"), &[], &[], &[], false)
        .expect("expands")
        .to_string();
    assert!(!tokens.contains("fn produced"), "{tokens}");
}

// --- the scalar table ------------------------------------------------------

/// One row per keyword, and the row is what every question about a scalar is
/// answered from. The test that matters is that the columns agree with each
/// other: a `doc` that is not the `rust` spelling with the module path taken
/// off is a doc line naming a type the caller cannot write.
#[test]
fn every_scalar_has_a_row_whose_columns_agree() {
    for keyword in [
        "string", "bool", "i32", "u32", "i53", "u53", "i64", "u64", "f64", "path", "json",
    ] {
        let scalar = Scalar::from_keyword(keyword).expect("a keyword this crate knows");
        let target = scalar_target(scalar);
        assert!(
            target.rust.ends_with(target.doc),
            "`{keyword}` documents `{}` but generates `{}`",
            target.doc,
            target.rust,
        );
        assert!(
            target.rust.parse::<TokenStream>().is_ok(),
            "`{keyword}` generates `{}`, which is not a type",
            target.rust,
        );
    }
}

/// The two rows where `ts_rs` reading the Rust alone gets it wrong, and the
/// nine where it does not. A safe integer generates an `i64`, which `ts_rs`
/// maps to `bigint` — correct for a value that may exceed the safe range and
/// wrong for one the contract says cannot; passthrough JSON is `unknown`
/// rather than a named import. Everything else is left alone deliberately:
/// an `i64` really is a `bigint`, and saying so is the warning the keyword
/// exists to give.
#[test]
fn only_the_scalars_rust_cannot_express_carry_a_typescript_override() {
    let overridden: Vec<&str> = [
        "string", "bool", "i32", "u32", "i53", "u53", "i64", "u64", "f64", "path", "json",
    ]
    .into_iter()
    .filter(|k| {
        scalar_target(Scalar::from_keyword(k).expect("a keyword"))
            .ts
            .is_some()
    })
    .collect();
    assert_eq!(overridden, ["i53", "u53", "json"]);
}
