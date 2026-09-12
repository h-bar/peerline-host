//! What `contract!` makes of a manifest.
//!
//! The claim the macro exists to support is that the Rust and the manifest
//! cannot disagree, so the tests worth having are the ones that pin *what*
//! the expansion says about the file — the op names, the kinds, the wire
//! spellings, and the shape of the constructors a caller will use. Nothing
//! here checks Rust for Rust's sake; each case is a way the expansion could
//! silently describe a different contract than the file does.

use peerline_contract::spec::{Manifest, OpKind};

mod demo {
    //! The fixture manifest, expanded.
    peerline_contract::contract!("demo.thing/2" in "tests/contracts");
}

const SOURCE: &str = include_str!("contracts/demo.thing.v2.json");

/// The op constants carry the manifest's full names, and `OPS` lists them in
/// the manifest's order — the list a producer registers from.
#[test]
fn the_ops_are_the_manifests_ops() {
    assert_eq!(demo::CONTRACT, "demo.thing");
    assert_eq!(demo::VERSION, 2);
    assert_eq!(demo::OP_OPEN, "demo.thing.open");
    assert_eq!(demo::OP_WATCH, "demo.thing.watch");
    assert_eq!(demo::OP_CLOSE, "demo.thing.close");
    assert_eq!(
        demo::OPS,
        ["demo.thing.open", "demo.thing.watch", "demo.thing.close"],
    );
}

/// The field no schema substitutes for. A producer registering `watch` with
/// `on_request` would answer a `call_stream` with a kind mismatch at runtime;
/// this table is what a registration test compares against.
#[test]
fn each_op_carries_whether_it_is_unary_or_a_stream() {
    assert_eq!(
        demo::OP_KINDS,
        [
            ("demo.thing.open", OpKind::Unary),
            ("demo.thing.watch", OpKind::Stream),
            ("demo.thing.close", OpKind::Unary),
        ],
    );
}

#[test]
fn the_error_code_is_the_manifests_code() {
    assert_eq!(demo::ERR_DEMO, 4242);
}

/// The generated types are the file, so every constant and every kind in the
/// expansion must still be in the manifest when the two are compared at
/// runtime. This is the check that catches the one failure the macro cannot
/// catch for itself: an expansion that outlived the file it came from.
#[test]
fn the_expansion_still_matches_the_file_it_came_from() {
    let manifest = Manifest::parse(SOURCE).expect("the fixture is a valid manifest");
    assert_eq!(manifest.contract, demo::CONTRACT);
    assert_eq!(manifest.version, demo::VERSION);
    let ops: Vec<(&str, OpKind)> = manifest
        .ops
        .iter()
        .map(|op| (op.name.as_str(), op.kind))
        .collect();
    assert_eq!(ops, demo::OP_KINDS.to_vec());
    assert_eq!(manifest.errors[0].code, demo::ERR_DEMO);
}

/// A struct's constructor takes exactly its required fields, leaves the
/// optional ones absent, and takes anything convertible where the manifest
/// said `string` or `path` — because every call site otherwise writes
/// `.to_string()` at it.
#[test]
fn a_constructor_takes_what_the_shape_requires_and_nothing_else() {
    let args = demo::OpenArgs::new("a label");
    assert_eq!(args.label, "a label");
    assert_eq!(args.root, None);
    assert_eq!(args.settings, None);
    assert_eq!(args.tags, None);
}

/// An absent optional field is absent on the wire, not null: that is the form
/// the manifest describes, and peerline rejects params that are not an
/// object, so a caller can send the minimum without sending four nulls.
#[test]
fn absent_optionals_are_omitted_rather_than_serialised_as_null() {
    let json = serde_json::to_value(demo::OpenArgs::new("a label")).expect("encode");
    let object = json.as_object().expect("params must be an object");
    assert_eq!(object.len(), 1, "{json}");
    assert!(object.contains_key("label"));
}

/// A shape built only from exact scalars carries total equality; the manifest
/// is what says which, so nobody has to remember. `OpenArgs` carries
/// passthrough JSON and so does not — it would not compile here.
#[test]
fn total_equality_follows_the_shape() {
    fn needs_eq<T: Eq>(_: &T) {}
    needs_eq(&demo::OpenResult::new("id", 3));
    needs_eq(&demo::WatchArgs::new("id"));
    needs_eq(&demo::Note::line("hello"));
    needs_eq(&demo::Severity::Info);
}

/// The wire spelling of a variant is the manifest's spelling, not whatever a
/// case conversion happens to produce from the Rust name.
#[test]
fn variants_serialise_under_the_names_the_manifest_gave_them() {
    let json = serde_json::to_value(demo::Severity::GoneAway).expect("encode");
    assert_eq!(json, serde_json::json!("gone_away"));

    let json = serde_json::to_value(demo::Note::line("hello")).expect("encode");
    assert_eq!(json, serde_json::json!({ "kind": "line", "text": "hello" }));

    let json = serde_json::to_value(demo::Note::done()).expect("encode");
    assert_eq!(json, serde_json::json!({ "kind": "done" }));

    let json = serde_json::to_value(demo::Note::level(demo::Severity::Info)).expect("encode");
    assert_eq!(
        json,
        serde_json::json!({ "kind": "level", "severity": "info" }),
    );
}

#[test]
fn a_union_round_trips_through_its_wire_form() {
    for note in [
        demo::Note::line("hello"),
        demo::Note::level(demo::Severity::GoneAway),
        demo::Note::done(),
    ] {
        let encoded = serde_json::to_string(&note).expect("encode");
        let decoded: demo::Note = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, note);
    }
}

/// Prose is not checked here: a doc comment leaves no runtime trace, so the
/// test that the manifest's paragraphs actually reach the expansion is a unit
/// test inside the macro crate, against the tokens it emits.
#[test]
fn every_named_shape_in_the_manifest_exists_in_the_expansion() {
    let manifest = Manifest::parse(SOURCE).expect("valid");
    // Naming each generated type in Rust is what proves it exists; the
    // manifest side of the comparison is the count, so a type added to the
    // file and not to this list fails here rather than going unnoticed.
    let generated: &[&str] = &[
        "OpenArgs",
        "OpenResult",
        "WatchArgs",
        "Severity",
        "Note",
        "Trouble",
    ];
    let declared: Vec<&str> = manifest.types.iter().map(|t| t.name()).collect();
    assert_eq!(declared, generated);
    // Each one, named: a type the macro failed to emit is a name that does
    // not resolve, so this block not compiling is the assertion.
    let _ = demo::OpenArgs::new("l");
    let _ = demo::OpenResult::new("id", 3);
    let _ = demo::WatchArgs::new("id");
    let _ = demo::Severity::Info;
    let _ = demo::Note::done();
    let _ = demo::Trouble::new(demo::Severity::Info, "detail");
}
