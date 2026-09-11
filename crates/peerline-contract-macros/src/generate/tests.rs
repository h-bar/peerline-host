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
    tokens(json, &[], &[], &[])
        .expect("the fixture expands")
        .to_string()
}

fn tokens(
    json: &str,
    imports: &[(&str, TokenStream)],
    supports: &[(&str, Support)],
    typescript: &[&str],
) -> syn::Result<TokenStream> {
    let manifest = Manifest::parse(json).expect("the fixture is a valid manifest");
    let imports: Vec<(LitStr, TokenStream)> = imports
        .iter()
        .map(|(id, path)| (LitStr::new(id, Span::call_site()), path.clone()))
        .collect();
    let supports: Vec<(Ident, Support)> = supports
        .iter()
        .map(|(name, level)| (format_ident!("{}", name), *level))
        .collect();
    let typescript: Vec<Ident> = typescript
        .iter()
        .map(|name| format_ident!("{}", name))
        .collect();
    let bindings = Bindings::resolve(&manifest, &imports, &supports, &typescript)?;
    contract(
        &manifest,
        &bindings,
        Path::new("/tmp/demo.v1.json"),
        Span::call_site(),
    )
}

fn expansion() -> String {
    expand(FIXTURE)
}

fn rejection(json: &str, imports: &[(&str, TokenStream)], supports: &[(&str, Support)]) -> String {
    tokens(json, imports, supports, &[])
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

/// Without this the compiled types outlive the file they came from — the
/// one failure a design whose claim is "they cannot drift" cannot afford.
#[test]
fn the_expansion_declares_the_manifest_as_a_source_input() {
    let tokens = expansion();
    assert!(tokens.contains("include_bytes"), "{tokens}");
    assert!(tokens.contains("\"/tmp/demo.v1.json\""), "{tokens}");
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

/// The manifest says `Shared`; the call site says where Rust keeps it. That
/// split is the point: the manifest stays readable by an implementor who has
/// never heard of a crate.
#[test]
fn an_imported_type_takes_the_path_the_call_site_gave_it() {
    let tokens = tokens(
        IMPORTING,
        &[("demo.vocab/1", quote!(::demo_vocab))],
        &[],
        &[],
    )
    .expect("expands")
    .to_string();
    assert!(tokens.contains(":: demo_vocab :: Shared"), "{tokens}");
}

/// An unmapped import cannot be generated at all, and the error says what to
/// write rather than leaving a type that does not resolve.
#[test]
fn an_import_the_call_site_did_not_map_is_a_compile_error() {
    let err = rejection(IMPORTING, &[], &[]);
    assert!(err.contains("which Rust module supplies"), "{err}");
}

/// The other direction. A mapping for something the manifest does not import
/// does nothing, which is how a renamed vocabulary would go unnoticed.
#[test]
fn a_mapping_the_manifest_does_not_declare_is_rejected() {
    let err = rejection(
        IMPORTING,
        &[
            ("demo.vocab/1", quote!(::demo_vocab)),
            ("demo.other/1", quote!(::demo_other)),
        ],
        &[],
    );
    assert!(err.contains("imports nothing of that name"), "{err}");
}

/// `supports` answers for shapes this expansion cannot see. A local shape is
/// not one of them, and letting the call site overrule the manifest there
/// would let the Rust say something the contract does not.
#[test]
fn supports_may_not_speak_about_a_local_shape() {
    let err = rejection(
        IMPORTING,
        &[("demo.vocab/1", quote!(::demo_vocab))],
        &[("Args", Support::None)],
    );
    assert!(err.contains("not an imported type"), "{err}");
}

/// The rungs, end to end: an imported shape with no equality drags its
/// container down to no derives at all, and one that is `copy` lets the
/// container be copied.
#[test]
fn an_imported_shapes_rung_decides_its_containers_derives() {
    let with = |level| {
        tokens(
            IMPORTING,
            &[("demo.vocab/1", quote!(::demo_vocab))],
            &[("Shared", level)],
            &[],
        )
        .expect("expands")
        .to_string()
    };

    let none = with(Support::None);
    assert!(!none.contains("PartialEq"), "{none}");

    let partial = with(Support::Partial);
    assert!(partial.contains("PartialEq"), "{partial}");
    assert!(!partial.contains("cmp :: Eq"), "{partial}");

    let total = with(Support::Total);
    assert!(total.contains("cmp :: Eq"), "{total}");
    assert!(!total.contains("marker :: Copy"), "{total}");

    let copy = with(Support::Copy);
    assert!(copy.contains("marker :: Copy"), "{copy}");
}

/// And the default, which is the assumption that fails loudly: an imported
/// shape nobody spoke about is assumed comparable, so a Rust type that is not
/// fails to compile naming the trait rather than comparing wrongly.
#[test]
fn an_imported_shape_nobody_spoke_about_is_assumed_totally_comparable() {
    let tokens = tokens(
        IMPORTING,
        &[("demo.vocab/1", quote!(::demo_vocab))],
        &[],
        &[],
    )
    .expect("expands")
    .to_string();
    assert!(tokens.contains("cmp :: Eq"), "{tokens}");
    assert!(!tokens.contains("marker :: Copy"), "{tokens}");
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
    let tokens = tokens(&json, &[("demo.vocab/1", quote!(::demo_vocab))], &[], &[])
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
    tokens(BROWSER, &[], &[], names)
        .expect("expands")
        .to_string()
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
    let err = tokens(BROWSER, &[], &[], &["Row"])
        .expect_err("Row holds a Kind")
        .to_string();
    assert!(err.contains("add `Kind` to the clause"), "{err}");
}

#[test]
fn the_clause_may_only_name_shapes_the_manifest_declares() {
    let err = tokens(BROWSER, &[], &[], &["Nonesuch"])
        .expect_err("there is no such shape")
        .to_string();
    assert!(err.contains("not a shape this manifest declares"), "{err}");

    let err = tokens(BROWSER, &[], &[], &["Kind", "Kind"])
        .expect_err("named twice")
        .to_string();
    assert!(err.contains("named twice"), "{err}");
}

/// An imported shape's Rust type belongs to the expansion of the manifest it
/// came from, so its TypeScript is asked for there.
#[test]
fn the_clause_may_not_name_an_imported_shape() {
    let manifest = Manifest::parse(IMPORTING).expect("valid");
    let imports = vec![(
        LitStr::new("demo.vocab/1", Span::call_site()),
        quote!(::demo_vocab),
    )];
    let err = match Bindings::resolve(&manifest, &imports, &[], &[format_ident!("Shared")]) {
        Ok(_) => panic!("`Shared` is imported, so the clause may not name it"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("imported from another manifest"), "{err}");
}
