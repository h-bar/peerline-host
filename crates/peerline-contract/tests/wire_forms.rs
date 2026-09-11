//! The wire forms the format grew keys for, asserted as literal JSON.
//!
//! Every assertion here compares `to_string` against the exact text, and not
//! a round trip. A round trip passes whenever both directions are wrong in
//! the same way, which is precisely the failure each of these keys exists to
//! rule out: a value spelled `gone_away` on both sides is a contract nobody
//! else can implement, and a field that writes `null` where the manifest
//! promised absence round-trips perfectly and breaks the decoder at the far
//! end.
//!
//! `contract_macro.rs` covers what the format could already say. This covers
//! what it could not.

use peerline_contract::spec::{Manifest, Schema};

mod vocab {
    //! A vocabulary, expanded. No ops, because nobody produces one.
    peerline_contract::contract!("tests/contracts/demo.vocab.v1.json");
}

mod wire {
    //! A contract that imports it.
    //!
    //! The manifest names `demo.vocab/1`; this call site is where Rust
    //! learns that the module holding it is `crate::vocab`, because which
    //! module that is a fact about this build and not about the contract.
    //!
    //! `supports` answers the one question the manifest cannot: `Meta` is
    //! declared in a file this expansion never opens, so how far up the
    //! comparison ladder the Rust type goes is not readable from here.
    peerline_contract::contract!(
        "tests/contracts/demo.wire.v1.json",
        imports {
            "demo.vocab/1" => crate::vocab,
        },
        supports {
            Meta: copy,
        },
    );
}

const SOURCE: &str = include_str!("contracts/demo.wire.v1.json");

fn text<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("encode")
}

/// B1. The manifest's `name` is the identifier every generator derives its
/// own spelling from; `wire` is the literal text. Before the two were
/// separate keys, the wire value had to be a legal `lower_snake_case` word —
/// one language's identifier rule imposed on the JSON.
#[test]
fn a_variants_wire_value_is_the_manifests_wire_value() {
    assert_eq!(text(&wire::Backend::ClaudeCode), r#""claude-code""#);
    assert_eq!(text(&wire::Backend::Codex), r#""codex""#);
    assert_eq!(text(&vocab::Direction::Input), r#""in""#);
    assert_eq!(text(&vocab::Direction::Output), r#""out""#);
}

/// And the decoder side: the wire value is what is accepted, and the
/// identifier is not.
#[test]
fn nothing_but_the_wire_value_decodes() {
    let decoded: wire::Backend = serde_json::from_str(r#""claude-code""#).expect("decode");
    assert_eq!(decoded, wire::Backend::ClaudeCode);
    serde_json::from_str::<wire::Backend>(r#""claude_code""#)
        .expect_err("the identifier is not on the wire");
}

/// B3. A nullable field writes its key every time, with `null` in it. An
/// optional one is not there at all. Both are in this manifest, and the
/// difference between them is the whole reason there are two keys.
#[test]
fn nullable_writes_null_and_optional_writes_nothing() {
    assert_eq!(
        text(&wire::Filter::default()),
        r#"{"since_ms":null,"kinds":null}"#,
    );
    assert_eq!(
        text(&wire::StartArgs::new(wire::Session::new(
            wire::Backend::Codex,
            "/tmp",
        ))),
        r#"{"backend":"codex","working_dir":"/tmp"}"#,
        "`max_event_ts` is optional, so it is absent rather than null",
    );
}

/// A nullable field still takes an absent key on the way in. A decoder that
/// tolerates less than it emits helps nobody.
#[test]
fn a_nullable_field_still_accepts_an_absent_key() {
    let decoded: wire::Filter = serde_json::from_str("{}").expect("decode");
    assert_eq!(decoded, wire::Filter::default());
}

/// B4. A default is what an absent key *means*, and the value is in the
/// manifest so that every implementor gets the same one. A reader who
/// guessed `false` for `include_history` would answer a different question
/// than the contract asks.
#[test]
fn an_absent_key_means_the_manifests_default_and_not_the_types() {
    let decoded: wire::ListParams = serde_json::from_str("{}").expect("decode");
    assert!(
        decoded.include_history,
        "`bool::default()` is false; the manifest says true",
    );
    assert_eq!(
        text(&decoded),
        r#"{"include_history":true,"filter":{"since_ms":null,"kinds":null}}"#,
    );
    // The same value, reached the other way: a shape whose every field has
    // an answer when nobody spoke has a canonical empty form.
    assert_eq!(wire::ListParams::default(), decoded);
    assert!(
        !serde_json::from_str::<wire::ListParams>(r#"{"include_history":false}"#)
            .expect("decode")
            .include_history
    );
}

/// B5. An externally tagged arm is a one-key object whose key is the arm and
/// whose value is the payload — which is the only way to carry a list, since
/// there is nowhere inside a JSON array to put a discriminator.
#[test]
fn an_external_union_is_one_key_and_its_payload() {
    assert_eq!(
        text(&wire::KindFilter::only(vec![
            wire::Backend::ClaudeCode,
            wire::Backend::Codex,
        ])),
        r#"{"only":["claude-code","codex"]}"#,
    );
    assert_eq!(text(&wire::KindFilter::except(vec![])), r#"{"except":[]}"#,);
}

/// B6. An internally tagged arm carrying a whole shape puts that shape's
/// keys beside the discriminator — the same bytes as spelling the fields out
/// in the variant, and a different Rust type: the shape stays one shape, so
/// a consumer holding one can put it on the stream and take it back off.
#[test]
fn an_internally_tagged_payload_sits_beside_the_tag() {
    let event = wire::Event::new(vocab::Meta::new(7, vocab::Direction::Output), "hello");
    let item = wire::Item::event(event.clone());
    assert_eq!(
        text(&item),
        r#"{"item":"event","meta":{"timestamp_ms":7,"direction":"out"},"line":"hello"}"#,
    );
    assert_eq!(item.as_event(), Some(&event));
    assert_eq!(text(&wire::Item::gap(3)), r#"{"item":"gap","lost":3}"#);
    assert_eq!(wire::Item::gap(3).as_event(), None);
}

/// B7. A flattened field has no key: its type's keys are written beside its
/// siblings. There is no `session` on this wire, which is what makes the
/// forwarded request the same bytes the caller sent.
#[test]
fn a_flattened_field_has_no_key_of_its_own() {
    let mut args = wire::StartArgs::new(wire::Session::new(wire::Backend::ClaudeCode, "/srv"));
    args.max_event_ts = Some(11);
    assert_eq!(
        text(&args),
        r#"{"backend":"claude-code","working_dir":"/srv","max_event_ts":11}"#,
    );
    let decoded: wire::StartArgs =
        serde_json::from_str(r#"{"backend":"claude-code","working_dir":"/srv","max_event_ts":11}"#)
            .expect("decode");
    assert_eq!(decoded.session.backend, wire::Backend::ClaudeCode);
}

/// B8. An op that sometimes answers nothing is not an op that answers
/// nothing. Absent `result` is the second thing; a nullable result is the
/// first, and the manifest can now tell them apart.
#[test]
fn a_result_may_be_nullable_without_being_absent() {
    let manifest = Manifest::parse(SOURCE).expect("valid");
    let op = |name: &str| {
        manifest
            .ops
            .iter()
            .find(|op| op.name == name)
            .unwrap_or_else(|| panic!("{name} is in the manifest"))
    };
    assert_eq!(
        op("demo.wire.latest").result,
        Some(Schema::Nullable {
            nullable: Box::new(Schema::Named("Event".into())),
        }),
    );
    assert_eq!(
        op("demo.wire.list").result.as_ref().map(list_of),
        Some(true)
    );
    // And the two are still distinct from an op that answers nothing at all,
    // which is what the watch op's absent result means.
    assert_eq!(op("demo.wire.watch").result, None);
}

fn list_of(schema: &Schema) -> bool {
    matches!(schema, Schema::Array { .. })
}

/// B2. The manifest names a contract and a version; the call site named a
/// Rust module. What arrives is one type, used by both expansions, so the
/// shape two contracts share is one declaration rather than two that can
/// drift.
#[test]
fn an_imported_shape_is_the_same_shape_on_both_sides() {
    let meta = vocab::Meta::new(1, vocab::Direction::Input);
    let event = wire::Event::new(meta, "line");
    assert_eq!(event.meta, meta);
    assert_eq!(
        text(&event),
        r#"{"meta":{"timestamp_ms":1,"direction":"in"},"line":"line"}"#,
    );

    let manifest = Manifest::parse(SOURCE).expect("valid");
    let import = &manifest.imports[0];
    assert_eq!(import.id(), "demo.vocab/1");
    assert_eq!(import.types, ["Meta"]);
    // Nothing in the file says `crate::vocab`, and that is the point.
    assert!(
        !SOURCE.contains("vocab::"),
        "the manifest names no Rust path"
    );

    // The cross-file check this crate can make once it holds both.
    let vocabulary = Manifest::parse(include_str!("contracts/demo.vocab.v1.json")).expect("valid");
    manifest
        .check_imports(std::slice::from_ref(&vocabulary))
        .expect("`Meta` is declared over there");
}

/// B9. The derives follow from the shape, so nobody has to remember — except
/// for an imported shape, where the call site's `supports` is the only
/// available answer. `Cursor` holds nothing but an imported `Meta` the call
/// site called `copy`, so it is copyable; `Event` holds a string and so is
/// not, whatever anybody said.
#[test]
fn the_derives_follow_the_shape_and_the_call_site() {
    fn needs_copy<T: Copy>(_: &T) {}
    fn needs_eq<T: Eq>(_: &T) {}

    let meta = vocab::Meta::new(2, vocab::Direction::Output);
    needs_copy(&meta);
    needs_copy(&wire::Cursor::new(meta));
    needs_eq(&wire::Filter::default());
    needs_eq(&wire::Event::new(meta, "line"));
    needs_eq(&wire::KindFilter::only(vec![]));
}
