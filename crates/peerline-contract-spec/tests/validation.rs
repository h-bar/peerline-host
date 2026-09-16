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

/// A default is what an *absent* key means, and a nullable key is never
/// absent on the way out — so the two would be two answers to one question.
#[test]
fn a_default_and_a_null_are_not_both_true_of_one_field() {
    let err = reject(&with_field(
        r#"{ "name": "id", "type": "string", "nullable": true, "default": "x",
             "description": "Which one." }"#,
    ));
    assert!(err.contains("two answers to one question"), "{err}");
    assert!(
        err.contains("`optional` with a `default` is the combination that means something"),
        "the message must point at the pair that *is* legal: {err}",
    );
}

/// The pair, accepted. `optional` says absence is legal on the wire and
/// `default` says what an absent key means; together they are the statement
/// serde spells `#[serde(default = …, skip_serializing_if = …)]`, which is
/// the commonest field shape in the wires this format describes and the one
/// it previously could not write down.
///
/// `peerline-cluster` is the recorded case: all six `SpawnDiagnostic` fields
/// are declared `optional` with the meaning of an absent key in the prose —
/// "absent means none", for a counter that is zero almost always "and is why
/// it is omitted rather than written out". That sentence is this pair, said
/// where no generator reads it.
#[test]
fn optional_and_default_together_are_a_state_rather_than_a_contradiction() {
    for field in [
        r#"{ "name": "dropped", "type": "u53", "optional": true, "default": 0,
             "description": "How many lines were discarded. Absent means none." }"#,
        r#"{ "name": "label", "type": "string", "optional": true, "default": "unnamed",
             "description": "What to call it." }"#,
        r#"{ "name": "level", "type": "Level", "optional": true, "default": "info",
             "description": "How bad." }"#,
        r#"{ "name": "tags", "type": { "array": "string" }, "optional": true, "default": [],
             "description": "Which." }"#,
        r#"{ "name": "inner", "type": "Inner", "optional": true, "default": {},
             "description": "Inside." }"#,
        r#"{ "name": "env", "type": { "map": "string" }, "optional": true, "default": {},
             "description": "Environment for this instance." }"#,
    ] {
        Manifest::parse(&with_field(field)).unwrap_or_else(|err| panic!("{field}: {err}"));
    }
}

/// The pair reaches a fifth [`FieldState`], which is how a checker in any
/// language is told by its own compiler that it has one more case to answer
/// rather than quietly folding it into `Default`.
#[test]
fn the_pair_is_its_own_field_state_and_the_plain_default_is_unmoved() {
    use peerline_contract_spec::{FieldState, TypeDef};

    let parsed = Manifest::parse(&with_field(
        r#"{ "name": "both", "type": "u32", "optional": true, "default": 0,
             "description": "Absent means none." },
           { "name": "just_default", "type": "u32", "default": 7,
             "description": "Always written." },
           { "name": "just_optional", "type": "u32", "optional": true,
             "description": "Absent means nothing was said." }"#,
    ))
    .expect("valid");
    let TypeDef::Struct { fields, .. } = &parsed.types[2] else {
        panic!("the third type is `Args`");
    };
    assert_eq!(
        fields[0].state(),
        FieldState::OptionalDefault(&serde_json::json!(0)),
    );
    assert_eq!(
        fields[1].state(),
        FieldState::Default(&serde_json::json!(7))
    );
    assert_eq!(fields[2].state(), FieldState::Optional);

    // The two halves each state answers, asked without matching on the arm.
    assert!(
        fields[0].state().may_be_absent(),
        "absent in both directions"
    );
    assert!(
        !fields[1].state().may_be_absent(),
        "a plain default is always written on the way out",
    );
    assert_eq!(
        fields[0].state().default_value(),
        Some(&serde_json::json!(0)),
    );
    assert_eq!(fields[2].state().default_value(), None);
    // And a field in the pair still requires nothing of a caller.
    assert!(fields.iter().all(|f| !f.is_required()));
}

/// The one thing the pair asks of a shape that no other state does. A writer
/// omits the key exactly when the value equals the default, so it has to
/// compare them — and the format's own `Scalar::is_exact` says `f64` and
/// passthrough `json` cannot be compared exactly. A generator handed such a
/// field would emit a predicate whose answer, and therefore whose wire form,
/// depends on a comparison the contract disclaims.
#[test]
fn the_pair_is_refused_where_the_shape_cannot_be_compared_exactly() {
    for (field, scalar) in [
        (
            r#"{ "name": "rate", "type": "f64", "optional": true, "default": 1.0,
                 "description": "How fast." }"#,
            "f64",
        ),
        (
            r#"{ "name": "raw", "type": "json", "optional": true, "default": {},
                 "description": "Passed through." }"#,
            "json",
        ),
        (
            r#"{ "name": "rates", "type": { "array": "f64" }, "optional": true, "default": [],
                 "description": "How fast, each." }"#,
            "f64",
        ),
        (
            r#"{ "name": "rates", "type": { "map": "f64" }, "optional": true, "default": {},
                 "description": "How fast, by name." }"#,
            "f64",
        ),
        // Through a named shape: the walk resolves it and finds the `f64`
        // the struct holds, so the field is refused naming the scalar rather
        // than the struct the author was looking at.
        (
            r#"{ "name": "fuzzy", "type": "Fuzzy", "optional": true, "default": {},
                 "description": "Inside." }"#,
            "f64",
        ),
    ] {
        let json = with_field(field).replace(
            r#"{ "form": "struct", "name": "Inner","#,
            r#"{ "form": "struct", "name": "Fuzzy", "description": "A shape holding a double.",
                 "fields": [ { "name": "rate", "type": "f64", "default": 1.0,
                               "description": "How fast." } ] },
               { "form": "struct", "name": "Inner","#,
        );
        let err = reject(&json);
        assert!(
            err.contains(&format!("holds a `{scalar}`")),
            "expected the message to name the `{scalar}`: {err}",
        );
        assert!(
            err.contains("not comparable for exact equality"),
            "and to say why: {err}",
        );
        assert!(
            err.contains("Say `default` alone"),
            "and what to write instead: {err}",
        );
    }
}

/// And the same shapes are perfectly legal in every other state, which is
/// what makes the rule a property of the pair rather than of the scalar.
#[test]
fn an_inexact_scalar_is_only_refused_in_the_one_state_that_compares_it() {
    for field in [
        r#"{ "name": "rate", "type": "f64", "default": 1.0, "description": "How fast." }"#,
        r#"{ "name": "rate", "type": "f64", "optional": true, "description": "How fast." }"#,
        r#"{ "name": "rate", "type": "f64", "nullable": true, "description": "How fast." }"#,
        r#"{ "name": "rate", "type": "f64", "description": "How fast." }"#,
    ] {
        Manifest::parse(&with_field(field)).unwrap_or_else(|err| panic!("{field}: {err}"));
    }
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
///
/// The defaulted field is `Inner` rather than `Args` itself. An earlier
/// spelling had `Args` hold an `Args`, which is a size cycle as well as an
/// incomplete default — and the manifest was refused for the cycle only once
/// the size check moved ahead of the walk over type bodies, which is where it
/// has to be: resolving a default declared on a field of a self-referential
/// shape follows the name to itself forever.
#[test]
fn a_struct_default_must_supply_what_the_struct_requires() {
    let json = with_field(
        r#"{ "name": "inner", "type": "Inner", "default": {}, "description": "Inside." }"#,
    )
    .replace(
        r#""fields": [ { "name": "count""#,
        r#""fields": [ { "name": "need", "type": "string", "description": "Needed." },
                       { "name": "count""#,
    );
    let err = reject(&json);
    assert!(err.contains("which `Inner` requires"), "{err}");
    assert!(err.contains("`need`"), "{err}");
}

/// And the cycle the old spelling of the test above accidentally described is
/// refused for being one, in a manifest that also declares a default on the
/// cycling field — which is the case that made the ordering load-bearing.
#[test]
fn a_size_cycle_is_found_before_a_default_on_the_cycling_field_is_resolved() {
    let json = with_field(
        r#"{ "name": "args", "type": "Args", "default": {}, "description": "Itself." }"#,
    );
    let err = reject(&json);
    assert!(err.contains("have no size"), "{err}");
    assert!(err.contains("Args -> Args"), "{err}");
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

// ---------------------------------------------------------------------------
// The safe-integer scalars
// ---------------------------------------------------------------------------

/// The keyword's whole content is a promise about the value, so a default
/// that breaks it is the manifest contradicting itself — and this is the one
/// place the format can see a value at all.
#[test]
fn a_default_outside_the_safe_integer_range_is_rejected() {
    for field in [
        r#"{ "name": "at_ms", "type": "i53", "default": 9007199254740992,
             "description": "When." }"#,
        r#"{ "name": "at_ms", "type": "i53", "default": -9007199254740992,
             "description": "When." }"#,
        r#"{ "name": "size", "type": "u53", "default": 9007199254740992,
             "description": "How much." }"#,
    ] {
        let err = reject(&with_field(field));
        assert!(err.contains("outside"), "{err}");
        assert!(err.contains("9007199254740991"), "{err}");
        assert!(
            err.contains("an `i64` or a `u64`"),
            "the message must name the escape hatch: {err}",
        );
    }
}

/// The boundary is inclusive, and the wide keywords still take whatever
/// their own range allows — keeping both pairs is what makes the narrow one
/// mean anything.
#[test]
fn the_bound_is_inclusive_and_the_wide_keywords_are_unaffected() {
    for field in [
        r#"{ "name": "at_ms", "type": "i53", "default": 9007199254740991,
             "description": "When." }"#,
        r#"{ "name": "at_ms", "type": "i53", "default": -9007199254740991,
             "description": "When." }"#,
        r#"{ "name": "size", "type": "u53", "default": 9007199254740991,
             "description": "How much." }"#,
        r#"{ "name": "size", "type": "u53", "default": 0, "description": "How much." }"#,
        r#"{ "name": "id", "type": "u64", "default": 9007199254740992,
             "description": "Somebody else's id." }"#,
    ] {
        Manifest::parse(&with_field(field)).unwrap_or_else(|err| panic!("{field}: {err}"));
    }
}

/// A negative default for an unsigned safe integer is the ordinary type
/// mismatch, not a range complaint: the two are different mistakes and the
/// message says which.
#[test]
fn a_wrongly_signed_safe_integer_is_a_type_mismatch_not_a_range_one() {
    let err = reject(&with_field(
        r#"{ "name": "size", "type": "u53", "default": -1, "description": "How much." }"#,
    ));
    assert!(err.contains("not a `u53`"), "{err}");
}

// ---------------------------------------------------------------------------
// Maps: the shape the format could not carry
// ---------------------------------------------------------------------------

/// The gap this form closes. `peerline-cluster`'s `SpawnArgs.env` is a map of
/// strings to strings in Rust and on the wire, and the only truthful
/// declaration available was passthrough `json` — which says the value is not
/// interpreted when it very much is, and leaves "object of strings to
/// strings" as prose an implementor has to enforce.
#[test]
fn a_map_of_strings_is_a_shape_the_schema_can_now_state() {
    let parsed = Manifest::parse(&with_field(
        r#"{ "name": "env", "type": { "map": "string" },
             "description": "Environment for this instance, keyed by variable name." }"#,
    ))
    .expect("a map is a schema");
    let peerline_contract_spec::TypeDef::Struct { fields, .. } = &parsed.types[2] else {
        panic!("the third type is `Args`");
    };
    let peerline_contract_spec::Schema::Map { map } = &fields[0].ty else {
        panic!("the field is a map");
    };
    assert_eq!(
        **map,
        peerline_contract_spec::Schema::Named("string".into())
    );
}

/// A map's value schema is checked to the same depth an array's element
/// schema is, so a misspelled scalar inside one fails loudly rather than
/// becoming a dangling reference.
#[test]
fn a_maps_value_schema_resolves_like_an_arrays_element_schema() {
    let err = reject(&with_field(
        r#"{ "name": "env", "type": { "map": "strnig" }, "description": "By name." }"#,
    ));
    assert!(err.contains("neither a scalar keyword"), "{err}");

    // Nested both ways, because a map of lists and a list of maps are both
    // ordinary compositions and neither needed a rule of its own.
    for ty in [
        r#"{ "map": { "array": "Level" } }"#,
        r#"{ "array": { "map": "Level" } }"#,
        r#"{ "map": { "map": "string" } }"#,
    ] {
        Manifest::parse(&with_field(&format!(
            r#"{{ "name": "env", "type": {ty}, "description": "By name." }}"#
        )))
        .unwrap_or_else(|err| panic!("{ty}: {err}"));
    }
}

/// A map value that is "a thing or null" is refused exactly where an array
/// element is: absent and null are a distinction only a *field* can draw, and
/// neither an element nor a map value is a field.
#[test]
fn a_map_value_may_not_be_a_nullable_schema() {
    let err = reject(&with_field(
        r#"{ "name": "env", "type": { "map": { "nullable": "string" } },
             "description": "By name." }"#,
    ));
    assert!(err.contains("a field says"), "{err}");
}

/// A map's default is a JSON object, and every value in it is checked against
/// the value schema. The keys are not checked against anything, because the
/// schema declares nothing about them beyond their being strings.
#[test]
fn a_map_default_type_checks_every_value_and_no_key() {
    for field in [
        r#"{ "name": "env", "type": { "map": "string" }, "default": {},
             "description": "By name." }"#,
        r#"{ "name": "env", "type": { "map": "string" },
             "default": { "HOME": "/root", "any-key-at-all": "x" },
             "description": "By name." }"#,
        r#"{ "name": "counts", "type": { "map": "u32" }, "default": { "a": 1 },
             "description": "By name." }"#,
    ] {
        Manifest::parse(&with_field(field)).unwrap_or_else(|err| panic!("{field}: {err}"));
    }

    let err = reject(&with_field(
        r#"{ "name": "env", "type": { "map": "string" }, "default": { "HOME": 7 },
             "description": "By name." }"#,
    ));
    assert!(err.contains("not a `string`"), "{err}");
    assert!(
        err.contains("`HOME`"),
        "the message must name the entry that is wrong: {err}",
    );

    let err = reject(&with_field(
        r#"{ "name": "env", "type": { "map": "string" }, "default": [],
             "description": "By name." }"#,
    ));
    assert!(err.contains("not an object"), "{err}");
}

/// Flattening a map is the one thing serde would happily do and no manifest
/// in this format means: it is a catch-all for every key the enclosing object
/// did not declare, which reopens a closed shape through the type of one
/// field. Refused, with the reason recorded rather than left to be inferred
/// from the general "only an object's keys" message.
#[test]
fn a_map_typed_field_may_not_be_flattened() {
    let err = reject(&with_field(
        r#"{ "name": "env", "type": { "map": "string" }, "flatten": true,
             "description": "By name." }"#,
    ));
    assert!(err.contains("flattened and is a map"), "{err}");
    assert!(
        err.contains("did not declare lands in here"),
        "the message must say what flattening a map would mean: {err}",
    );
    assert!(
        err.contains("accepts anything"),
        "and why that is not a thing this format declares: {err}",
    );
}

/// peerline rejects params that are not a JSON object, and a map *is* one —
/// but it is one with no declared keys, which is not a set of arguments
/// anybody can call. The same rule a list meets, and the same message.
#[test]
fn a_maps_position_rules_are_an_arrays() {
    let json = manifest("").replace(r#""args": "Args""#, r#""args": { "map": "string" }"#);
    assert!(
        reject(&json).contains("must be a struct"),
        "{}",
        reject(&json)
    );

    // A result, an item and an error's data take one as readily as they take
    // a list, because a map is a value like any other.
    for (from, to) in [
        (
            r#""args": "Args","#,
            r#""args": "Args", "result": { "map": "string" },"#,
        ),
        (
            r#""kind": "unary", "args": "Args","#,
            r#""kind": "stream", "args": "Args", "item": { "map": "string" },"#,
        ),
    ] {
        Manifest::parse(&manifest("").replace(from, to))
            .unwrap_or_else(|err| panic!("{to}: {err}"));
    }
}

/// An internally tagged arm has nowhere safe to put its discriminator inside
/// a map: the keys are the contract's *data*, they are not declared, and a
/// producer whose map happened to hold the tag's own key would emit an object
/// a decoder reads as a different arm. The message says which tagging takes
/// it instead, as the list and scalar cases already do.
#[test]
fn an_internally_tagged_arm_may_not_carry_a_map() {
    let err = reject(&with_union(
        r#"{ "form": "union", "name": "Item", "tag": "kind", "description": "One item.",
             "variants": [ { "name": "line", "payload": { "map": "string" },
                             "description": "Something was said." } ] }"#,
    ));
    assert!(err.contains("payload is a map"), "{err}");
    assert!(err.contains("could collide with one"), "{err}");
    assert!(err.contains(r#""tagging": "external""#), "{err}");

    // Externally tagged, it is fine: the arm's name is the key and the map is
    // its whole value, which is the same reason external tagging exists for a
    // list.
    Manifest::parse(&with_union(
        r#"{ "form": "union", "name": "Item", "tagging": "external",
             "description": "One item.",
             "variants": [ { "name": "line", "payload": { "map": "string" },
                             "description": "Something was said." } ] }"#,
    ))
    .expect("an external arm may carry a map");
}

/// A map breaks a size cycle exactly as a list does, and for the same reason:
/// the values are behind an indirection.
#[test]
fn recursion_through_a_map_is_allowed() {
    let json = r#"{
        "contract": "demo", "version": 1, "description": "A contract.",
        "types": [
            { "form": "struct", "name": "Node", "description": "A node.",
              "fields": [ { "name": "children", "type": { "map": "Node" },
                            "description": "Its children, by name." } ] },
            { "form": "struct", "name": "Args", "description": "Args.",
              "fields": [] }
        ],
        "ops": [ { "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Go." } ]
    }"#;
    Manifest::parse(json).expect("a map is indirection enough");
}

// ---------------------------------------------------------------------------
// Newtypes: a name for a shape that is already something else
// ---------------------------------------------------------------------------

/// One manifest whose `{TYPES}` are spelled per test, plus the `Args` every
/// op needs.
fn with_types(types: &str) -> String {
    format!(
        r#"{{
            "contract": "demo", "version": 1,
            "description": "A contract for testing the rules.",
            "types": [
                {types},
                {{ "form": "struct", "name": "Args", "description": "What a call carries.",
                   "fields": [ {{ "name": "id", "type": "string",
                                 "description": "Which one." }} ] }}
            ],
            "ops": [
                {{ "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." }}
            ]
        }}"#
    )
}

/// The form, and what it is for: `peerline-cluster`'s `HostId` is a string,
/// its dialling and its socket-reachability check are both keyed on the type,
/// and a manifest that could only say `string` described a contract whose
/// Rust cannot be written the way the hand-written Rust already is.
#[test]
fn a_newtype_is_a_name_for_a_shape_that_is_already_something_else() {
    let parsed = Manifest::parse(&with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": "string",
             "description": ["Which host, in the coordinator's namespace."] }"#,
    ))
    .expect("a newtype is a type definition");
    let peerline_contract_spec::TypeDef::Newtype {
        name,
        newtype,
        description,
    } = &parsed.types[0]
    else {
        panic!("the first type is the newtype");
    };
    assert_eq!(name, "HostId");
    assert_eq!(
        *newtype,
        peerline_contract_spec::Schema::Named("string".into())
    );
    assert_eq!(
        description.lines(),
        vec!["Which host, in the coordinator's namespace."],
    );
}

/// It is a type definition like any other, so it answers to every rule they
/// all answer to: a name, a description, and one declaration per name.
#[test]
fn a_newtype_obeys_the_rules_every_type_definition_obeys() {
    let err = reject(&with_types(
        r#"{ "form": "newtype", "name": "hostId", "newtype": "string",
             "description": "Which host." }"#,
    ));
    assert!(err.contains("must be PascalCase"), "{err}");

    let err = reject(&with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": "string", "description": "  " }"#,
    ));
    assert!(err.contains("no description"), "{err}");

    let err = reject(&with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": "string",
             "description": "Which host." },
           { "form": "newtype", "name": "HostId", "newtype": "u32",
             "description": "Which host, again." }"#,
    ));
    assert!(err.contains("declared twice"), "{err}");
}

/// The inner schema sits where a struct field's type sits, so it gets a
/// struct field's rules: it must resolve, and it may not be a nullable
/// *schema* — "or null" about a named thing belongs at the place the name is
/// used, where a reader can see it, rather than baked into the name so that
/// every use of it is quietly optional.
#[test]
fn a_newtypes_inner_schema_follows_a_struct_fields_position_rules() {
    let err = reject(&with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": "strnig",
             "description": "Which host." }"#,
    ));
    assert!(err.contains("neither a scalar keyword"), "{err}");

    let err = reject(&with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": { "nullable": "string" },
             "description": "Which host." }"#,
    ));
    assert!(err.contains("a field says"), "{err}");

    // And the compositions that *are* a field's are all fine.
    for inner in [
        r#""string""#,
        r#""u53""#,
        r#"{ "array": "string" }"#,
        r#"{ "map": "string" }"#,
        r#""Level""#,
    ] {
        Manifest::parse(&with_types(&format!(
            r#"{{ "form": "enum", "name": "Level", "description": "How bad.",
                  "variants": [ {{ "name": "info", "description": "Fine." }} ] }},
               {{ "form": "newtype", "name": "HostId", "newtype": {inner},
                  "description": "Which host." }}"#
        )))
        .unwrap_or_else(|err| panic!("{inner}: {err}"));
    }
}

/// A newtype holds its inner shape by value and adds nothing, so a newtype of
/// itself — or a ring of them — has no size, exactly as a struct containing
/// itself does not. The same message, because it is the same fact.
#[test]
fn a_newtype_may_not_wrap_itself_directly_or_through_a_ring() {
    let err = reject(&with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": "HostId",
             "description": "Which host." }"#,
    ));
    assert!(err.contains("have no size"), "{err}");

    let err = reject(&with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": "NodeId",
             "description": "Which host." },
           { "form": "newtype", "name": "NodeId", "newtype": "HostId",
             "description": "Which node." }"#,
    ));
    assert!(err.contains("have no size"), "{err}");

    // A list or a map in the ring breaks it, which is what the message says
    // to do.
    Manifest::parse(&with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": { "array": "HostId" },
             "description": "Which hosts." }"#,
    ))
    .expect("a list is indirection enough here too");
}

/// A newtype is wire-identical to what it wraps, so a default of one is a
/// default of the inner shape and is checked as that.
#[test]
fn a_default_of_a_newtype_is_checked_against_what_it_wraps() {
    let with_host_id = |field: &str| {
        with_types(&format!(
            r#"{{ "form": "newtype", "name": "HostId", "newtype": "string",
                  "description": "Which host." }},
               {{ "form": "struct", "name": "Row", "description": "One row.",
                  "fields": [ {field} ] }}"#
        ))
    };
    Manifest::parse(&with_host_id(
        r#"{ "name": "host", "type": "HostId", "default": "local",
             "description": "Where. Absent means this machine." }"#,
    ))
    .expect("a string is a `HostId`");

    let err = reject(&with_host_id(
        r#"{ "name": "host", "type": "HostId", "default": 7, "description": "Where." }"#,
    ));
    assert!(err.contains("not a `string`"), "{err}");
}

/// peerline rejects params that are not an object, and a newtype of a string
/// is a string. The same rule an enum meets, and the same message.
#[test]
fn a_newtype_may_not_be_an_ops_args() {
    let json = with_types(
        r#"{ "form": "newtype", "name": "HostId", "newtype": "string",
             "description": "Which host." }"#,
    )
    .replace(r#""args": "Args""#, r#""args": "HostId""#);
    assert!(
        reject(&json).contains("must be a struct"),
        "{}",
        reject(&json)
    );
}

/// Whether an internally tagged arm can carry a newtype is entirely a
/// question about the inner shape, so it is answered by asking it — a newtype
/// of a struct is as carryable as the struct, and a newtype of a string is as
/// uncarryable as the string.
#[test]
fn an_internally_tagged_arm_carries_a_newtype_exactly_as_it_carries_the_inner_shape() {
    let union = |inner: &str| {
        with_types(&format!(
            r#"{{ "form": "struct", "name": "Inner", "description": "An inner shape.",
                  "fields": [ {{ "name": "count", "type": "u32",
                                "description": "How many." }} ] }},
               {{ "form": "newtype", "name": "Wrapped", "newtype": {inner},
                  "description": "A name for it." }},
               {{ "form": "union", "name": "Item", "tag": "kind", "description": "One item.",
                  "variants": [ {{ "name": "line", "payload": "Wrapped",
                                  "description": "Something was said." }} ] }}"#
        ))
    };
    Manifest::parse(&union(r#""Inner""#)).expect("a newtype of a struct is an object");

    let err = reject(&union(r#""string""#));
    assert!(err.contains("nowhere to put the discriminator"), "{err}");

    let err = reject(&union(r#"{ "map": "string" }"#));
    assert!(err.contains("payload is a map"), "{err}");
}

// ---------------------------------------------------------------------------
// Error-code bands
// ---------------------------------------------------------------------------

/// One manifest whose errors and `codes` key are spelled per test.
fn with_codes(codes: &str, errors: &str) -> String {
    format!(
        r#"{{
            "contract": "demo", "version": 1,
            "description": "A contract for testing the rules.",
            {codes}
            "types": [
                {{ "form": "struct", "name": "Args", "description": "What a call carries.",
                   "fields": [] }}
            ],
            "errors": [ {errors} ],
            "ops": [
                {{ "name": "demo.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." }}
            ]
        }}"#
    )
}

const BAND: &str = r#""codes": { "start": 3100, "end": 3199 },"#;

/// A code inside the band this contract claims is the ordinary case, and both
/// ends are inclusive — an exclusive end would make the natural spelling of a
/// hundred-wide band end on a digit belonging to the next one.
#[test]
fn a_code_inside_the_band_is_accepted_and_both_ends_are_inclusive() {
    for code in [3100, 3150, 3199] {
        Manifest::parse(&with_codes(
            BAND,
            &format!(r#"{{ "name": "one", "code": {code}, "description": "It failed." }}"#),
        ))
        .unwrap_or_else(|err| panic!("{code}: {err}"));
    }
}

/// The band is the contract's own claim about its numbers, so a code outside
/// it is the manifest contradicting itself — caught in the file that made the
/// claim rather than as a collision in whichever directory it later joins.
#[test]
fn a_code_outside_the_band_names_the_manifest_the_code_and_the_band() {
    for code in [3099, 3200, 1] {
        let err = reject(&with_codes(
            BAND,
            &format!(r#"{{ "name": "one", "code": {code}, "description": "It failed." }}"#),
        ));
        assert!(err.contains("`demo/1`"), "{err}");
        assert!(err.contains(&format!("code {code}")), "{err}");
        assert!(err.contains("3100..=3199"), "{err}");
        assert!(err.contains("outside the band"), "{err}");
    }
}

/// A band whose end is below its start holds nothing, which is a claim on
/// nothing.
#[test]
fn a_band_that_holds_no_codes_is_rejected() {
    let err = reject(&with_codes(
        r#""codes": { "start": 3200, "end": 3100 },"#,
        r#"{ "name": "one", "code": 3150, "description": "It failed." }"#,
    ));
    assert!(err.contains("end is below its start"), "{err}");
    assert!(err.contains("claim on nothing"), "{err}");
}

/// A band of one code is legal: a contract with a single error that means to
/// reserve exactly its own number has said something true.
#[test]
fn a_band_of_one_code_is_a_band() {
    Manifest::parse(&with_codes(
        r#""codes": { "start": 3101, "end": 3101 },"#,
        r#"{ "name": "one", "code": 3101, "description": "It failed." }"#,
    ))
    .expect("a band may be one code wide");
}

/// Absent is today's behaviour exactly, which is what lets a band be added to
/// a directory one file at a time.
#[test]
fn a_manifest_with_no_band_is_unaffected_by_every_rule_the_key_adds() {
    let parsed = Manifest::parse(&with_codes(
        "",
        r#"{ "name": "one", "code": 7, "description": "It failed." },
           { "name": "two", "code": 999999, "description": "It also failed." }"#,
    ))
    .expect("codes without a band are checked for uniqueness and nothing else");
    assert!(parsed.codes.is_none());
}

/// A band reserves codes for the errors a producer raises, and a vocabulary
/// has no producer — so the claim would hold numbers out of every other
/// contract's reach on behalf of nobody. The same reasoning one step on from
/// the rule that a vocabulary declares no errors.
#[test]
fn a_vocabulary_may_not_claim_a_band() {
    let json = VOCABULARY.replace(r#""types": ["#, &format!("{BAND} \"types\": ["));
    let err = reject(&json);
    assert!(err.contains("is a vocabulary"), "{err}");
    assert!(err.contains("claims the error-code band"), "{err}");
    assert!(err.contains("has no producer"), "{err}");
}

/// An unknown key inside the band is rejected like every other unknown key: a
/// `"step": 10` somebody wrote is a thing they believe is being honoured.
#[test]
fn an_unknown_key_in_the_band_is_rejected_rather_than_ignored() {
    let err = reject(&with_codes(
        r#""codes": { "start": 3100, "end": 3199, "step": 10 },"#,
        r#"{ "name": "one", "code": 3101, "description": "It failed." }"#,
    ));
    assert!(err.contains("unknown field"), "{err}");
}

// ---------------------------------------------------------------------------
// Bands across a directory, which is where the partition actually lives
// ---------------------------------------------------------------------------

use peerline_contract_spec::Directory;

/// One contract, named and banded per call, with one error at `code`.
fn banded(contract: &str, codes: &str, code: i32) -> Manifest {
    let json = format!(
        r#"{{
            "contract": "{contract}", "version": 1,
            "description": "A contract for testing the rules.",
            {codes}
            "types": [
                {{ "form": "struct", "name": "Args", "description": "What a call carries.",
                   "fields": [] }}
            ],
            "errors": [ {{ "name": "one", "code": {code}, "description": "It failed." }} ],
            "ops": [
                {{ "name": "{contract}.go", "kind": "unary", "args": "Args",
                   "description": "Do the thing." }}
            ]
        }}"#
    );
    Manifest::parse(&json).unwrap_or_else(|err| panic!("{contract}: {err}"))
}

fn refuse(manifests: Vec<Manifest>) -> String {
    Directory::new(manifests)
        .expect_err("this directory must be refused")
        .to_string()
}

/// The partition, checked. Four contracts at 3001, 3101, 3201 and 3301, each
/// claiming the hundred it sits in — which is exactly what `coagent`'s
/// directory does today in four hand-chosen literals whose descriptions each
/// assert the code is "distinct from every other code in this system". Four
/// true sentences, no checker; this is the checker.
#[test]
fn bands_that_do_not_overlap_are_a_partition_a_loader_can_hold() {
    Directory::new(vec![
        banded(
            "storage",
            r#""codes": { "start": 3001, "end": 3100 },"#,
            3001,
        ),
        banded(
            "server",
            r#""codes": { "start": 3101, "end": 3200 },"#,
            3101,
        ),
        banded(
            "runtime",
            r#""codes": { "start": 3201, "end": 3300 },"#,
            3201,
        ),
        banded(
            "manager",
            r#""codes": { "start": 3301, "end": 3400 },"#,
            3301,
        ),
    ])
    .expect("four hundreds, side by side, touching nowhere");
}

/// The check that fires early. Two claimed ranges may not overlap even where
/// no number in them is spent yet, so the failure lands on whoever picks an
/// overlapping range — while they can still pick a different one — rather
/// than on whoever later mounts both contracts.
#[test]
fn two_overlapping_bands_are_refused_naming_both_contracts_and_both_bands() {
    let err = refuse(vec![
        banded(
            "storage",
            r#""codes": { "start": 3001, "end": 3100 },"#,
            3001,
        ),
        banded(
            "server",
            r#""codes": { "start": 3050, "end": 3150 },"#,
            3060,
        ),
    ]);
    assert!(err.contains("`storage/1`"), "{err}");
    assert!(err.contains("`server/1`"), "{err}");
    assert!(err.contains("3001..=3100"), "{err}");
    assert!(err.contains("3050..=3150"), "{err}");
    assert!(err.contains("overlap"), "{err}");
}

/// Touching at one end is overlapping. A band whose `end` is the next band's
/// `start` shares that code, and both ends are inclusive.
#[test]
fn bands_that_share_a_single_code_overlap() {
    let err = refuse(vec![
        banded(
            "storage",
            r#""codes": { "start": 3001, "end": 3100 },"#,
            3001,
        ),
        banded(
            "server",
            r#""codes": { "start": 3100, "end": 3199 },"#,
            3100,
        ),
    ]);
    assert!(err.contains("overlap"), "{err}");
}

/// A band that wholly contains another is the same failure, and the order the
/// manifests arrive in does not change whether it is found.
#[test]
fn a_band_inside_another_is_refused_from_either_direction() {
    for pair in [
        (
            r#""codes": { "start": 3000, "end": 3999 },"#,
            r#""codes": { "start": 3100, "end": 3199 },"#,
        ),
        (
            r#""codes": { "start": 3100, "end": 3199 },"#,
            r#""codes": { "start": 3000, "end": 3999 },"#,
        ),
    ] {
        let err = refuse(vec![
            banded("storage", pair.0, 3150),
            banded("server", pair.1, 3150),
        ]);
        // Both spend 3150, so the duplicate-code check would also have caught
        // this pair; what is asserted is that *a* refusal happens from either
        // order, which the band check guarantees even when no number is spent.
        assert!(!err.is_empty(), "{err}");
    }
    // And with nothing spent in common, only the band check can see it.
    let err = refuse(vec![
        banded(
            "storage",
            r#""codes": { "start": 3000, "end": 3999 },"#,
            3000,
        ),
        banded(
            "server",
            r#""codes": { "start": 3100, "end": 3199 },"#,
            3100,
        ),
    ]);
    assert!(err.contains("overlap"), "{err}");
}

/// Banded and unbanded manifests coexist, which is what lets a band be added
/// to a directory one file at a time — and the cost of that is stated rather
/// than hidden: an unbanded manifest's codes are not held out of anybody's
/// band, because a manifest that claimed no range made no promise about where
/// its numbers would be.
#[test]
fn a_banded_and_an_unbanded_manifest_coexist() {
    Directory::new(vec![
        banded(
            "storage",
            r#""codes": { "start": 3001, "end": 3100 },"#,
            3001,
        ),
        banded("legacy", "", 9999),
    ])
    .expect("a manifest that claims no range is checked as it always was");

    // Including where the unbanded one's code sits inside the banded one's
    // range. Refusing this would make adding a band to one file a breaking
    // change to every other file in the directory.
    Directory::new(vec![
        banded(
            "storage",
            r#""codes": { "start": 3001, "end": 3100 },"#,
            3001,
        ),
        banded("legacy", "", 3050),
    ])
    .expect("an unbanded manifest made no promise about where its numbers are");
}

/// And the rule the bands were added beside is unmoved: two contracts picking
/// the same number is still a mis-decode, banded or not.
#[test]
fn the_duplicate_code_check_is_unchanged_by_bands() {
    let err = refuse(vec![banded("storage", "", 7), banded("server", "", 7)]);
    assert!(err.contains("error code 7 is declared by"), "{err}");

    // Inside one band it cannot happen — a contract's own codes are unique
    // within the file — so the cross-contract case is the whole of it.
    let err = refuse(vec![
        banded(
            "storage",
            r#""codes": { "start": 3001, "end": 3100 },"#,
            3001,
        ),
        banded("legacy", "", 3001),
    ]);
    assert!(err.contains("error code 3001 is declared by"), "{err}");
}
