//! What a manifest may not say.
//!
//! Every case here is a way a contract could be wrong that no generator would
//! catch on its own — either because it would produce code that compiles and
//! misbehaves, or because each generator would catch it differently and the
//! contract would then exist in one language and not another.

use peerline_contract_spec::Manifest;

/// The smallest manifest that is legal, with `{EXTRA}` replaced per test.
fn manifest(extra: &str) -> String {
    format!(
        r#"{{
            "contract": "demo", "version": 1,
            "description": "A contract for testing the rules.",
            "types": [
                {{ "form": "struct", "name": "Args",
                  "description": "What a call carries.",
                  "fields": [ {{ "name": "id", "type": "string",
                                "description": "Which one." }} ] }}
            ],
            "ops": [
                {{ "name": "demo.go", "kind": "unary", "args": "Args",
                  "description": "Do the thing." }}
                {extra}
            ]
        }}"#
    )
}

fn reject(json: &str) -> String {
    Manifest::parse(json)
        .expect_err("this manifest must be rejected")
        .to_string()
}

#[test]
fn the_baseline_manifest_is_accepted() {
    let parsed = Manifest::parse(&manifest("")).expect("valid");
    assert_eq!(parsed.contract, "demo");
    assert_eq!(parsed.ops.len(), 1);
}

/// The whole point of the format. A shape with no explanation is a shape
/// whose reasoning stayed in somebody's head, and a warning is how the prose
/// fails to move.
#[test]
fn a_shape_with_no_description_is_not_a_finished_manifest() {
    let json = r#"{
        "contract": "demo", "version": 1,
        "description": "A contract.",
        "types": [ { "form": "struct", "name": "Args", "description": "Args.",
                     "fields": [ { "name": "id", "type": "string" } ] } ],
        "ops": [ { "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Go." } ]
    }"#;
    assert!(reject(json).contains("description"), "{}", reject(json));
}

#[test]
fn an_empty_description_is_no_description() {
    let json = manifest("").replace(r#""Do the thing.""#, r#""   ""#);
    assert!(
        reject(&json).contains("no description"),
        "{}",
        reject(&json)
    );
}

/// The manifest is read by implementors whose doc tooling renders neither
/// rustdoc's `[`Type`]` nor markdown links. Letting them in would make the
/// shared file quietly Rust-shaped.
#[test]
fn rustdoc_link_syntax_is_rejected_because_the_prose_is_plain() {
    let json = manifest("").replace(r#""Do the thing.""#, r#""Do the thing. See [`Args`]. ""#);
    let err = reject(&json);
    assert!(err.contains("plain prose"), "{err}");

    let json = manifest("").replace(
        r#""Do the thing.""#,
        r#""Do the thing. See [the docs](http://example.invalid). ""#,
    );
    let err = reject(&json);
    assert!(err.contains("plain prose"), "{err}");
}

/// `kind` is the field no schema substitutes for, so the shape it implies is
/// enforced rather than assumed: a "stream" that answers with a result would
/// be read as unary by whoever wrote the producer.
#[test]
fn a_stream_must_emit_items_and_must_not_answer_with_a_result() {
    let json = manifest("").replace(r#""kind": "unary""#, r#""kind": "stream""#);
    let err = reject(&json);
    assert!(err.contains("must declare the `item`"), "{err}");

    let json = json.replace(
        r#""description": "Do the thing.""#,
        r#""item": "Args", "result": "Args", "description": "Do the thing.""#,
    );
    let err = reject(&json);
    assert!(err.contains("not a value"), "{err}");
}

#[test]
fn a_unary_op_must_not_emit_items() {
    let json = manifest("").replace(
        r#""description": "Do the thing.""#,
        r#""item": "Args", "description": "Do the thing.""#,
    );
    let err = reject(&json);
    assert!(err.contains("only a stream emits items"), "{err}");
}

/// peerline rejects params that are not a JSON object, so an op whose args
/// are a scalar is one nobody can call. Caught in the format rather than in
/// each language's test suite.
#[test]
fn args_must_be_a_struct() {
    let json = manifest("").replace(r#""args": "Args""#, r#""args": "string""#);
    let err = reject(&json);
    assert!(err.contains("must be a struct"), "{err}");
}

/// A misspelled scalar becomes a reference to a type that does not exist,
/// which is why references are resolved rather than trusted.
#[test]
fn a_dangling_type_reference_is_rejected() {
    let json = manifest("").replace(r#""type": "string""#, r#""type": "strnig""#);
    let err = reject(&json);
    assert!(err.contains("neither a scalar keyword"), "{err}");
}

/// Two contracts on one peer must not collide, and the op constant a
/// generator derives comes from the part after the prefix.
#[test]
fn an_op_outside_the_contracts_namespace_is_rejected() {
    let json = manifest("").replace(r#""demo.go""#, r#""other.go""#);
    let err = reject(&json);
    assert!(err.contains("not namespaced"), "{err}");
}

/// A consumer matches on the code and then decodes `data`, so two shapes
/// behind one code is a mis-decode waiting to happen.
#[test]
fn two_errors_may_not_share_a_code() {
    let json = r#"{
        "contract": "demo", "version": 1, "description": "A contract.",
        "types": [ { "form": "struct", "name": "Args", "description": "Args.",
                     "fields": [] } ],
        "errors": [
            { "name": "one", "code": 7, "description": "The first." },
            { "name": "two", "code": 7, "description": "The second." }
        ],
        "ops": [ { "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Go." } ]
    }"#;
    let err = reject(json);
    assert!(err.contains("used by two errors"), "{err}");
}

/// An op that raises an error nobody declared would generate a reference to a
/// constant that does not exist — a compile failure in Rust, and silence in a
/// language that has no constants.
#[test]
fn an_op_may_only_raise_a_declared_error() {
    let json = manifest("").replace(
        r#""description": "Do the thing.""#,
        r#""errors": ["nope"], "description": "Do the thing.""#,
    );
    let err = reject(&json);
    assert!(err.contains("not in this contract's `errors`"), "{err}");
}

/// A type that contains itself by value has no size. The message says to put
/// a list in the cycle rather than leaving the author to work it out from a
/// generated-code error.
#[test]
fn a_type_that_contains_itself_by_value_is_rejected() {
    let json = r#"{
        "contract": "demo", "version": 1, "description": "A contract.",
        "types": [
            { "form": "struct", "name": "Node", "description": "A node.",
              "fields": [ { "name": "next", "type": "Node",
                            "description": "The next one." } ] },
            { "form": "struct", "name": "Args", "description": "Args.",
              "fields": [] }
        ],
        "ops": [ { "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Go." } ]
    }"#;
    let err = reject(json);
    assert!(err.contains("have no size"), "{err}");
}

/// The same shape through a list is fine, and the message above says so.
#[test]
fn recursion_through_a_list_is_allowed() {
    let json = r#"{
        "contract": "demo", "version": 1, "description": "A contract.",
        "types": [
            { "form": "struct", "name": "Node", "description": "A node.",
              "fields": [ { "name": "children", "type": { "array": "Node" },
                            "description": "Its children." } ] },
            { "form": "struct", "name": "Args", "description": "Args.",
              "fields": [] }
        ],
        "ops": [ { "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Go." } ]
    }"#;
    Manifest::parse(json).expect("a list is indirection enough");
}

/// A field named the same as its union's tag would be overwritten by the tag
/// on the way out — serialised as one key, decoded as the other.
#[test]
fn a_union_variant_may_not_shadow_the_tag() {
    let json = r#"{
        "contract": "demo", "version": 1, "description": "A contract.",
        "types": [
            { "form": "union", "name": "Event", "tag": "kind",
              "description": "What happened.",
              "variants": [ { "name": "went", "description": "It went.",
                              "fields": [ { "name": "kind", "type": "string",
                                            "description": "Which." } ] } ] },
            { "form": "struct", "name": "Args", "description": "Args.",
              "fields": [] }
        ],
        "ops": [ { "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Go." } ]
    }"#;
    let err = reject(json);
    assert!(err.contains("union's own tag"), "{err}");
}

/// A field the format does not know is a field the author believes is being
/// honoured. Rejected rather than ignored.
#[test]
fn an_unknown_field_is_rejected_rather_than_ignored() {
    let json = manifest("").replace(
        r#""description": "Do the thing.""#,
        r#""stream": true, "description": "Do the thing.""#,
    );
    let err = reject(&json);
    assert!(err.contains("unknown field"), "{err}");
}

/// Prose is one string or a list of lines, and the two mean the same thing —
/// so the readable form in a file full of paragraphs costs nothing.
#[test]
fn prose_reads_the_same_either_way() {
    let one = Manifest::parse(&manifest("")).expect("valid");
    let other = Manifest::parse(&manifest("").replace(
        r#""A contract for testing the rules.""#,
        r#"["A contract for testing", "the rules."]"#,
    ))
    .expect("valid");
    assert_eq!(
        one.description.lines(),
        vec!["A contract for testing the rules."],
    );
    assert_eq!(
        other.description.lines(),
        vec!["A contract for testing", "the rules."],
    );
}

// ---------------------------------------------------------------------------
// `wire`: the identifier and the value on the wire are two different strings
// ---------------------------------------------------------------------------

/// One manifest with one enum, spelled `{VARIANTS}`.
fn with_enum(variants: &str) -> String {
    format!(
        r#"{{
            "contract": "demo", "version": 1,
            "description": "A contract for testing the rules.",
            "types": [
                {{ "form": "enum", "name": "Backend", "description": "Which one.",
                   "variants": [ {variants} ] }},
                {{ "form": "struct", "name": "Args", "description": "What a call carries.",
                   "fields": [ {{ "name": "backend", "type": "Backend",
                                 "description": "Which one." }} ] }}
            ],
            "ops": [
                {{ "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." }}
            ]
        }}"#
    )
}

/// The reason `wire` exists. A wire value is a JSON string and a JSON string
/// is not an identifier; requiring it to be a legal `lower_snake_case` word
/// imposed one language's identifier rules on the wire, in a file whose whole
/// premise is that it is not one language's.
#[test]
fn a_wire_value_may_be_any_string_while_the_name_stays_an_identifier() {
    let parsed = Manifest::parse(&with_enum(
        r#"{ "name": "claude_code", "wire": "claude-code", "description": "One." },
           { "name": "codex", "description": "The other." }"#,
    ))
    .expect("a kebab-case wire value is a legal wire value");
    let peerline_contract_spec::TypeDef::Enum { variants, .. } = &parsed.types[0] else {
        panic!("the first type is the enum");
    };
    assert_eq!(variants[0].wire(), "claude-code");
    // Absent means the two are the same, which is what every manifest
    // written before the key existed assumed.
    assert_eq!(variants[1].wire(), "codex");
}

/// `name` is what every generator derives its own spelling from, so it stays
/// the one shape they can all agree on. The kebab goes in `wire`.
#[test]
fn a_variant_name_is_still_an_identifier_even_when_wire_is_not() {
    let err = reject(&with_enum(
        r#"{ "name": "claude-code", "description": "One." }"#,
    ));
    assert!(err.contains("lower_snake_case"), "{err}");
    assert!(err.contains("`wire`"), "{err}");
}

/// Two arms that travel as one string are two arms a decoder cannot tell
/// apart, which no amount of distinct naming fixes.
#[test]
fn two_variants_may_not_travel_as_the_same_string() {
    let err = reject(&with_enum(
        r#"{ "name": "one", "wire": "same", "description": "One." },
           { "name": "two", "wire": "same", "description": "The other." }"#,
    ));
    assert!(err.contains("both travel as `same`"), "{err}");
}

#[test]
fn a_blank_wire_value_is_not_a_wire_value() {
    let err = reject(&with_enum(
        r#"{ "name": "one", "wire": "  ", "description": "One." }"#,
    ));
    assert!(err.contains("blank"), "{err}");
}

// ---------------------------------------------------------------------------
// Vocabularies and imports
// ---------------------------------------------------------------------------

const VOCABULARY: &str = r#"{
    "contract": "demo.vocab", "version": 1, "role": "vocabulary",
    "description": "Shapes that travel in more than one contract.",
    "types": [
        { "form": "struct", "name": "Shared", "description": "The shape.",
          "fields": [ { "name": "id", "type": "string",
                        "description": "Which one." } ] }
    ]
}"#;

fn importing(imports: &str) -> String {
    format!(
        r#"{{
            "contract": "demo", "version": 1,
            "description": "A contract for testing the rules.",
            "imports": [ {imports} ],
            "types": [
                {{ "form": "struct", "name": "Args", "description": "What a call carries.",
                   "fields": [ {{ "name": "shared", "type": "Shared",
                                 "description": "The shared shape." }} ] }}
            ],
            "ops": [
                {{ "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." }}
            ]
        }}"#
    )
}

const IMPORT: &str = r#"{ "contract": "demo.vocab", "version": 1, "types": ["Shared"],
                          "description": "Both contracts carry this shape." }"#;

/// A file whose types several contracts share. Nobody produces it, so it has
/// no ops — and before this role existed, a file with no ops was rejected as
/// "a name, not a contract", which it still is when it claims to be one.
#[test]
fn a_vocabulary_is_types_with_no_ops() {
    let parsed = Manifest::parse(VOCABULARY).expect("a vocabulary needs no ops");
    assert!(parsed.ops.is_empty());
    Manifest::parse(&importing(IMPORT)).expect("an imported name resolves");
}

#[test]
fn a_contract_with_no_ops_is_still_not_a_contract() {
    let json = VOCABULARY.replace(r#""role": "vocabulary","#, "");
    let err = reject(&json);
    assert!(err.contains("not a contract"), "{err}");
    assert!(err.contains("vocabulary"), "{err}");
}

/// An op is something a producer answers, and nobody produces a vocabulary.
#[test]
fn a_vocabulary_may_not_declare_ops() {
    let json = VOCABULARY.replace(
        r#""types": ["#,
        r#""ops": [ { "name": "demo.vocab.go", "kind": "unary",
                      "description": "Do the thing." } ], "types": ["#,
    );
    let err = reject(&json);
    assert!(err.contains("nobody produces a vocabulary"), "{err}");
}

/// Same reasoning one step on: an error code is something a producer raises.
#[test]
fn a_vocabulary_may_not_declare_errors() {
    let json = VOCABULARY.replace(
        r#""types": ["#,
        r#""errors": [ { "name": "nope", "code": 5, "description": "It failed." } ],
           "types": ["#,
    );
    let err = reject(&json);
    assert!(err.contains("has no producer"), "{err}");
}

/// An import is a coupling between two contracts, and the reason for one is
/// contract documentation like every other reason in the file.
#[test]
fn an_import_needs_a_reason_like_everything_else() {
    let err = reject(&importing(
        r#"{ "contract": "demo.vocab", "version": 1, "types": ["Shared"] }"#,
    ));
    assert!(err.contains("description"), "{err}");
}

#[test]
fn an_import_that_names_no_types_brings_nothing_into_scope() {
    let err = reject(&importing(
        r#"{ "contract": "demo.vocab", "version": 1, "types": [],
             "description": "Nothing." }"#,
    ));
    assert!(err.contains("brings nothing"), "{err}");
}

/// Type names share one namespace, so two versions of one vocabulary in one
/// manifest would be two shapes answering to one name.
#[test]
fn one_vocabulary_may_not_be_imported_at_two_versions() {
    let err = reject(&importing(&format!(
        r#"{IMPORT}, {{ "contract": "demo.vocab", "version": 2, "types": ["Other"],
                        "description": "The newer one." }}"#
    )));
    assert!(err.contains("imported twice"), "{err}");
}

#[test]
fn a_name_may_not_be_both_declared_and_imported() {
    let err = reject(&importing(
        r#"{ "contract": "demo.vocab", "version": 1, "types": ["Args"],
             "description": "Both contracts carry this shape." }"#,
    ));
    assert!(err.contains("declared here and also imported"), "{err}");
}

#[test]
fn a_contract_may_not_import_from_itself() {
    let err = reject(&importing(
        r#"{ "contract": "demo", "version": 1, "types": ["Shared"],
             "description": "Itself." }"#,
    ));
    assert!(err.contains("imports from itself"), "{err}");
}

/// One file cannot see another, so the check that an imported name exists is
/// the one check that needs both in hand. A tool that has loaded a directory
/// runs it; Rust gets the same answer from its own compiler, because a name
/// that is not there does not resolve.
#[test]
fn an_imported_name_is_checked_against_the_manifest_it_came_from() {
    let vocabulary = Manifest::parse(VOCABULARY).expect("valid");
    let importer = Manifest::parse(&importing(IMPORT)).expect("valid");
    importer
        .check_imports(std::slice::from_ref(&vocabulary))
        .expect("`Shared` is declared over there");

    let wrong = Manifest::parse(&importing(
        r#"{ "contract": "demo.vocab", "version": 1, "types": ["Shared", "Missing"],
             "description": "Both contracts carry this shape." }"#,
    ))
    .expect("nothing in one file says otherwise");
    let err = wrong
        .check_imports(std::slice::from_ref(&vocabulary))
        .expect_err("`Missing` is not over there")
        .to_string();
    assert!(err.contains("declares no such type"), "{err}");

    let err = importer
        .check_imports(&[])
        .expect_err("the vocabulary was not given")
        .to_string();
    assert!(err.contains("not among the manifests given"), "{err}");
}

// ---------------------------------------------------------------------------
// The three field states, and defaults
// ---------------------------------------------------------------------------

fn with_field(field: &str) -> String {
    format!(
        r#"{{
            "contract": "demo", "version": 1,
            "description": "A contract for testing the rules.",
            "types": [
                {{ "form": "enum", "name": "Level", "description": "How bad.",
                   "variants": [ {{ "name": "info", "description": "Fine." }} ] }},
                {{ "form": "struct", "name": "Inner", "description": "An inner shape.",
                   "fields": [ {{ "name": "count", "type": "u32", "default": 3,
                                 "description": "How many." }} ] }},
                {{ "form": "struct", "name": "Args", "description": "What a call carries.",
                   "fields": [ {field} ] }}
            ],
            "ops": [
                {{ "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." }}
            ]
        }}"#
    )
}

/// Absent and null are two different things on the wire, and a decoder in
/// another language needs to know which one this field means. Saying both is
/// saying neither.
#[test]
fn a_field_is_absent_or_null_but_not_both() {
    let err = reject(&with_field(
        r#"{ "name": "id", "type": "string", "optional": true, "nullable": true,
             "description": "Which one." }"#,
    ));
    assert!(err.contains("absent and null are the two answers"), "{err}");
}

/// A default is what an absent key *means*, so the field is neither absent
/// nor null once it has been read.
#[test]
fn a_default_and_an_absence_are_not_both_true_of_one_field() {
    let err = reject(&with_field(
        r#"{ "name": "id", "type": "string", "optional": true, "default": "x",
             "description": "Which one." }"#,
    ));
    assert!(err.contains("neither absent nor null"), "{err}");
}

/// The point of validating a default: one that a generator could not emit is
/// a manifest that produces different code in different targets, or none.
#[test]
fn a_default_must_be_a_value_of_the_field_s_type() {
    for (field, expected) in [
        (
            r#"{ "name": "id", "type": "string", "default": 7, "description": "Which." }"#,
            "not a `string`",
        ),
        (
            r#"{ "name": "n", "type": "u32", "default": -1, "description": "How many." }"#,
            "not a `u32`",
        ),
        (
            r#"{ "name": "n", "type": "i32", "default": 99999999999, "description": "How many." }"#,
            "not a `i32`",
        ),
        (
            r#"{ "name": "level", "type": "Level", "default": "nope", "description": "How bad." }"#,
            "one of `Level`'s values",
        ),
        (
            r#"{ "name": "tags", "type": { "array": "string" }, "default": "x",
                 "description": "Which." }"#,
            "not a list",
        ),
        (
            r#"{ "name": "inner", "type": "Inner", "default": { "nope": 1 },
                 "description": "Inside." }"#,
            "not a field of `Inner`",
        ),
    ] {
        let err = reject(&with_field(field));
        assert!(err.contains(expected), "expected `{expected}`, got: {err}");
    }
}

/// And the ones that are. A struct default leans on the struct's own
/// defaults, which is what makes `{}` a complete value rather than an empty
/// one.
#[test]
fn a_default_of_the_right_shape_is_accepted() {
    for field in [
        r#"{ "name": "id", "type": "string", "default": "x", "description": "Which." }"#,
        r#"{ "name": "on", "type": "bool", "default": true, "description": "Whether." }"#,
        r#"{ "name": "level", "type": "Level", "default": "info", "description": "How bad." }"#,
        r#"{ "name": "tags", "type": { "array": "string" }, "default": [],
             "description": "Which." }"#,
        r#"{ "name": "inner", "type": "Inner", "default": {}, "description": "Inside." }"#,
        r#"{ "name": "raw", "type": "json", "default": { "anything": [1] },
             "description": "Passed through." }"#,
    ] {
        Manifest::parse(&with_field(field)).unwrap_or_else(|err| panic!("{field}: {err}"));
    }
}

/// A default has to be checkable where it is written, and an imported shape
/// is not in this file.
#[test]
fn a_default_may_not_name_an_imported_shape() {
    let json = importing(IMPORT).replace(
        r#""type": "Shared","#,
        r#""type": "Shared", "default": {},"#,
    );
    let err = reject(&json);
    assert!(err.contains("cannot be checked from here"), "{err}");
}

/// A struct's default must satisfy the struct: a field with nothing to fall
/// back on has to be in there.
#[test]
fn a_struct_default_must_supply_what_the_struct_requires() {
    let json = with_field(
        r#"{ "name": "args", "type": "Args", "default": {}, "description": "Itself." }"#,
    )
    .replace(
        r#""fields": [ { "name": "args""#,
        r#""fields": [ { "name": "need", "type": "string", "description": "Needed." },
                       { "name": "args""#,
    );
    let err = reject(&json);
    assert!(err.contains("which `Args` requires"), "{err}");
}

// ---------------------------------------------------------------------------
// `flatten`
// ---------------------------------------------------------------------------

/// A flattened field contributes its type's keys, so only a type that *has*
/// keys can be flattened.
#[test]
fn only_an_object_can_be_flattened() {
    for (field, expected) in [
        (
            r#"{ "name": "id", "type": "string", "flatten": true, "description": "Which." }"#,
            "only an object's keys",
        ),
        (
            r#"{ "name": "tags", "type": { "array": "string" }, "flatten": true,
                 "description": "Which." }"#,
            "only an object's keys",
        ),
        (
            r#"{ "name": "level", "type": "Level", "flatten": true, "description": "How bad." }"#,
            "not a struct",
        ),
    ] {
        let err = reject(&with_field(field));
        assert!(err.contains(expected), "expected `{expected}`, got: {err}");
    }
}

/// A flattened field has no key of its own, so there is nothing for absence,
/// nullness or a default to be about.
#[test]
fn a_flattened_field_has_nothing_for_optional_to_mean() {
    let err = reject(&with_field(
        r#"{ "name": "inner", "type": "Inner", "flatten": true, "optional": true,
             "description": "Inside." }"#,
    ));
    assert!(err.contains("no key of its own"), "{err}");
}

/// Two fields that serialise as one key: the collision the wire form makes
/// possible and a field list alone would not.
#[test]
fn a_flattened_shape_may_not_collide_with_its_siblings() {
    let err = reject(&with_field(
        r#"{ "name": "inner", "type": "Inner", "flatten": true, "description": "Inside." },
           { "name": "count", "type": "u32", "description": "How many." }"#,
    ));
    assert!(err.contains("writes the key `count` twice"), "{err}");
}

// ---------------------------------------------------------------------------
// Unions: two taggings, and an arm that carries one whole shape
// ---------------------------------------------------------------------------

fn with_union(union: &str) -> String {
    format!(
        r#"{{
            "contract": "demo", "version": 1,
            "description": "A contract for testing the rules.",
            "types": [
                {{ "form": "enum", "name": "Level", "description": "How bad.",
                   "variants": [ {{ "name": "info", "description": "Fine." }} ] }},
                {{ "form": "struct", "name": "Inner", "description": "An inner shape.",
                   "fields": [ {{ "name": "count", "type": "u32",
                                 "description": "How many." }} ] }},
                {union},
                {{ "form": "struct", "name": "Args", "description": "What a call carries.",
                   "fields": [ {{ "name": "item", "type": "Item",
                                 "description": "The item." }} ] }}
            ],
            "ops": [
                {{ "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." }}
            ]
        }}"#
    )
}

/// `{"only":[…]}` is a real wire form and internal tagging cannot express
/// it: there is nowhere inside a JSON array to put a discriminator key.
#[test]
fn an_externally_tagged_union_carries_a_payload_that_is_not_an_object() {
    Manifest::parse(&with_union(
        r#"{ "form": "union", "name": "Item", "tagging": "external",
             "description": "Which rows pass.",
             "variants": [
               { "name": "only", "payload": { "array": "Level" },
                 "description": "Keep only these." },
               { "name": "except", "payload": { "array": "Level" },
                 "description": "Drop these." } ] }"#,
    ))
    .expect("an external union may carry a list");
}

#[test]
fn an_external_union_has_no_tag_field_to_name() {
    let err = reject(&with_union(
        r#"{ "form": "union", "name": "Item", "tagging": "external", "tag": "kind",
             "description": "Which rows pass.",
             "variants": [ { "name": "only", "payload": { "array": "Level" },
                             "description": "Keep only these." } ] }"#,
    ));
    assert!(err.contains("the arm's name is the key"), "{err}");
}

#[test]
fn an_internal_union_must_name_its_tag() {
    let err = reject(&with_union(
        r#"{ "form": "union", "name": "Item", "description": "One item.",
             "variants": [ { "name": "line", "description": "Something was said." } ] }"#,
    ));
    assert!(err.contains("must name the `tag` field"), "{err}");
}

/// An arm carries one shape or a set of keys. Both is two answers to the
/// question of what its payload is.
#[test]
fn an_arm_carries_a_payload_or_fields_and_not_both() {
    let err = reject(&with_union(
        r#"{ "form": "union", "name": "Item", "tag": "kind", "description": "One item.",
             "variants": [ { "name": "line", "payload": "Inner",
                             "fields": [ { "name": "text", "type": "string",
                                           "description": "What." } ],
                             "description": "Something was said." } ] }"#,
    ));
    assert!(err.contains("one shape or a set of keys"), "{err}");
}

/// Internal tagging puts the discriminator inside the payload's own object,
/// so the payload has to be an object. The error says which tagging would
/// take it instead.
#[test]
fn an_internally_tagged_arm_needs_somewhere_to_put_the_tag() {
    for payload in [r#"{ "array": "Level" }"#, r#""string""#, r#""Level""#] {
        let err = reject(&with_union(&format!(
            r#"{{ "form": "union", "name": "Item", "tag": "kind", "description": "One item.",
                  "variants": [ {{ "name": "line", "payload": {payload},
                                  "description": "Something was said." }} ] }}"#
        )));
        assert!(err.contains("nowhere to put the discriminator"), "{err}");
    }
}

/// The tag and a payload key would be one key on the wire — the same
/// collision a variant's own field list already could not have.
#[test]
fn an_internally_tagged_payload_may_not_shadow_the_tag() {
    let err = reject(&with_union(
        r#"{ "form": "union", "name": "Item", "tag": "count", "description": "One item.",
             "variants": [ { "name": "line", "payload": "Inner",
                             "description": "Something was said." } ] }"#,
    ));
    assert!(err.contains("which is the union's own tag"), "{err}");
}

/// An arm that carries a whole shape is still an arm: it may not travel as
/// the same string as another.
#[test]
fn a_payload_arm_resolves_like_any_other_reference() {
    let err = reject(&with_union(
        r#"{ "form": "union", "name": "Item", "tag": "kind", "description": "One item.",
             "variants": [ { "name": "line", "payload": "Nope",
                             "description": "Something was said." } ] }"#,
    ));
    assert!(err.contains("neither a scalar keyword"), "{err}");
}

// ---------------------------------------------------------------------------
// Nullable schemas: where a value has no field to say it with
// ---------------------------------------------------------------------------

/// An op that answers "the session, or nothing" answers something. Absent
/// `result` means it answers nothing every time, which is a different
/// contract.
#[test]
fn an_op_result_may_be_nullable() {
    let json = manifest("").replace(
        r#""description": "Do the thing.""#,
        r#""result": { "nullable": "Args" }, "description": "Do the thing.""#,
    );
    Manifest::parse(&json).expect("a result that is sometimes null is a result");
}

/// One spelling per idea. A field has to distinguish absent from null and a
/// nullable schema cannot, so the field keys are the only way to say it
/// there.
#[test]
fn a_field_says_nullable_with_its_own_key_and_not_with_a_schema() {
    let err = reject(&with_field(
        r#"{ "name": "id", "type": { "nullable": "string" }, "description": "Which one." }"#,
    ));
    assert!(err.contains("a field says"), "{err}");
}

/// Params are an object or nothing; "an object or null" is not a thing
/// peerline can carry.
#[test]
fn args_may_not_be_nullable() {
    let json = manifest("").replace(r#""args": "Args""#, r#""args": { "nullable": "Args" }"#);
    let err = reject(&json);
    assert!(err.contains("a field says"), "{err}");
}
