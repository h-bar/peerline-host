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
    assert!(err.contains("neither a scalar keyword nor a type"), "{err}");
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
