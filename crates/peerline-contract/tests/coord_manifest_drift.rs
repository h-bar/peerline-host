//! `coord/1` and `peerline.contract/1` are hand-written Rust and a manifest at
//! the same time. This is what stops the two from disagreeing.
//!
//! Everywhere else in this stack the manifest is the source and the Rust is
//! generated, so the two cannot drift. `coord/1` is the considered exception:
//! its types are not only wire shapes, they are the public API of a library —
//! `Host` takes a `Vec<Produced>`, a supervisor consumes a `Listing`, a
//! consumer holds a `Binding` — and two crates each expanding their own copy
//! could not pass those values to one another. So the Rust stays hand-written,
//! the manifest in `contracts/` is documentation of record, and this test is
//! the thing that makes the exception safe rather than merely cheap.
//!
//! # What it catches
//!
//! Every check below is mechanical: it serialises a real Rust value and asks
//! the manifest what it should have seen.
//!
//! - **A field added, removed or renamed on either side.** The key set of a
//!   serialised value must equal the manifest's declared field names exactly,
//!   and a mismatch names the type and the field.
//! - **A field re-optioned on either side.** The manifest's three states are
//!   checked against what serde actually does with a missing key: a `required`
//!   field must make decoding *fail* when it is absent, an `optional` one must
//!   decode and must be omitted rather than written as `null`, and a
//!   `default` one must decode to exactly the value the manifest declares.
//!   Adding `#[serde(default)]` to a required field, or dropping it from a
//!   defaulted one, fails here.
//! - **A default whose value changed.** The declared JSON default is compared
//!   against what the Rust type actually produces for an absent key, so
//!   `cardinality` silently defaulting to `single` would fail naming the field.
//! - **A union retagged or a variant respelled.** The discriminator key and
//!   each arm's literal wire value are read off the serialised Rust value and
//!   compared with the manifest, so `#[serde(tag = "status")]` becoming
//!   `tag = "state"`, or `rename_all` changing `unknown_instance` to
//!   `unknownInstance`, fails naming the union and the arm.
//! - **An op or an error code that moved.** Op names, op kinds, argument,
//!   result and item type names, error codes and error data shapes are checked
//!   against the `OP_*` and `ERR_*` constants, and the constant *names* a Rust
//!   generator would derive from the manifest are checked against the names
//!   this crate actually declares.
//! - **An op or error added to, or removed from, the manifest.** The declared
//!   sets must equal the pinned tables below.
//! - **A newtype that stopped being transparent.** `HostId` is declared a
//!   newtype over `string`, and the one property that makes that a description
//!   rather than a change is that it adds nothing to the bytes. The Rust value
//!   is compared against the bare JSON string in both directions, because a
//!   round trip cannot see the difference — a wrapper that invented an
//!   envelope would encode and decode its own invention perfectly.
//! - **A manifest that no longer validates**, or a directory whose files
//!   disagree with each other: `Directory::load` runs every rule in the format
//!   including the cross-file ones. That now includes `coord/1`'s declared
//!   code band: every error code in the file must be inside 1000..=1099, and
//!   no other manifest in this directory may claim a number in it.
//!
//! # What it does not catch
//!
//! Stated rather than left to be discovered, because an exception that is
//! trusted for more than it checks is worse than no exception.
//!
//! - **Semantics.** Nothing here reads the prose. A field correctly named,
//!   correctly optional and documented as meaning the opposite of what the
//!   coordinator does with it passes every assertion on this page.
//! - **An op or a type added to the Rust side alone.** Rust exposes no runtime
//!   list of its own constants or modules, so the op and error tables here are
//!   written out by hand and pinned to the constants by *value*. A new
//!   `OP_SOMETHING` that nobody adds to the table and to the manifest is
//!   invisible. The reverse direction — added to the manifest, absent from the
//!   table — does fail.
//! - **Integer width and range.** `u53` and `u64` both generate `u64` in Rust
//!   and are the same bytes in JSON, so this test cannot tell them apart. The
//!   millisecond fields are declared `u53`, which is a claim that the value
//!   stays inside ±(2^53 − 1) and is what lets a JavaScript implementor use a
//!   plain number; nothing at runtime enforces it, in either language.
//! - **Scalar keyword against Rust type**, for the same reason: `path` and
//!   `string` are both a JSON string, and `Endpoints.uds` is declared `path`
//!   while the hand-written Rust spells it `String`. See the note in
//!   `contracts/README.md` for what that would mean for a migration.
//! - **Anything the format has no key for.** The environment variables, the
//!   default socket path, the host-id derivation and the order the ops must be
//!   called in are all prose in the manifest and the README, and prose is not
//!   checkable from here. They are the reason the README exists.
//! - **Union arms and enum values added on the Rust side.** The arm lists are
//!   pinned tables here too, for the same reason the op table is.

use std::collections::BTreeSet;
use std::path::PathBuf;

use peerline_contract::coord::{
    self, AwaitReadyArgs, BindingGraph, Edge, Heartbeat, HeartbeatResult, InstanceInfo, Listing,
    ReadyArgs, ReadyResult, RegisterAck, RegisterArgs, ResolveArgs, ResolveResult,
    ServiceEndpoints, ServiceEntry, SingletonOccupied, Unresolved, UpdateAck, UpdateArgs,
    WatchEvent,
};
use peerline_contract::spec::{
    Directory, Field, FieldState, Manifest, OpKind, Schema, Tagging, TypeDef, const_name,
};
use peerline_contract::{
    Binding, Cardinality, Consumed, ContractId, Endpoints, HostId, IrohDial, Produced,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// The contract this file is about, and the vocabulary it imports from.
const COORD: &str = "coord";
const VOCABULARY: &str = "peerline.contract";

/// The directory holding both, relative to this crate.
///
/// At the root of the repository rather than under a crate, because it is not
/// an input to any Rust build — nothing expands it — and it is what a
/// non-Rust implementor clones.
fn contracts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts")
}

fn directory() -> Directory {
    Directory::load(&contracts_dir())
        .unwrap_or_else(|err| panic!("the contracts directory does not load: {err}"))
}

fn manifest<'a>(dir: &'a Directory, contract: &str) -> &'a Manifest {
    dir.get(contract, 1)
        .unwrap_or_else(|| panic!("the contracts directory holds no `{contract}/1`"))
}

// ---------------------------------------------------------------- the checks

/// The heart of it: one serialised Rust value against one list of declared
/// fields.
///
/// `tag` is the discriminator key of the enclosing union, which is a key on
/// the wire but not a field of the arm, and is `None` for a plain struct.
fn check_object<T>(label: &str, fields: &[Field], value: &T, tag: Option<&str>)
where
    T: Serialize + DeserializeOwned,
{
    let json = serde_json::to_value(value)
        .unwrap_or_else(|err| panic!("{label}: the Rust value does not serialise: {err}"));
    let Value::Object(map) = &json else {
        panic!("{label}: the Rust value serialises as `{json}`, which is not a JSON object");
    };

    let mut declared: BTreeSet<String> = fields.iter().map(|f| f.name.clone()).collect();
    if let Some(tag) = tag {
        declared.insert(tag.to_string());
    }
    let present: BTreeSet<String> = map.keys().cloned().collect();
    let missing: Vec<&String> = declared.difference(&present).collect();
    assert!(
        missing.is_empty(),
        "{label}: the manifest declares {missing:?}, which the Rust type does not write. A field \
         was added to the manifest, or removed or renamed in Rust."
    );
    let extra: Vec<&String> = present.difference(&declared).collect();
    assert!(
        extra.is_empty(),
        "{label}: the Rust type writes {extra:?}, which the manifest does not declare. A field \
         was added in Rust, or removed or renamed in the manifest."
    );

    for field in fields {
        let here = format!("{label}.{}", field.name);
        let mut without = map.clone();
        without.remove(&field.name);
        let absent = serde_json::from_value::<T>(Value::Object(without));
        match field.state() {
            FieldState::Required => assert!(
                absent.is_err(),
                "{here} is declared required, but the Rust type decodes an object that omits it. \
                 It is optional or defaulted in Rust and the manifest has not been told."
            ),
            FieldState::Optional => {
                assert!(
                    absent.is_ok(),
                    "{here} is declared optional, but the Rust type refuses an object that omits \
                     it. It is required in Rust."
                );
            }
            FieldState::Nullable => {
                assert!(
                    absent.is_ok(),
                    "{here} is declared nullable, and a nullable field still accepts an absent \
                     key on the way in, but the Rust type refuses one."
                );
                let mut nulled = map.clone();
                nulled.insert(field.name.clone(), Value::Null);
                assert!(
                    serde_json::from_value::<T>(Value::Object(nulled)).is_ok(),
                    "{here} is declared nullable, but the Rust type refuses a null value for it."
                );
            }
            FieldState::Default(expected) => {
                let decoded = absent.unwrap_or_else(|err| {
                    panic!(
                        "{here} is declared with a default, but the Rust type refuses an object \
                         that omits it: {err}. It is required in Rust."
                    )
                });
                let back = serde_json::to_value(&decoded)
                    .unwrap_or_else(|err| panic!("{here}: re-serialising failed: {err}"));
                assert_eq!(
                    back.get(&field.name),
                    Some(expected),
                    "{here}: the manifest says an absent key means `{expected}`, and the Rust \
                     type reads it as `{}`.",
                    back.get(&field.name)
                        .map_or_else(|| "nothing at all".to_string(), |v| v.to_string())
                );
            }
            // Both halves of the pair, because the difference between this and
            // `Default` is a *direction* and only one of them is about reading.
            // No `coord/1` field is in this state today; the arm is here so
            // that one arriving is checked rather than waved through, and so
            // that this test stops compiling if the format grows a sixth state.
            FieldState::OptionalDefault(expected) => {
                let decoded = absent.unwrap_or_else(|err| {
                    panic!(
                        "{here} is declared with a default, but the Rust type refuses an object \
                         that omits it: {err}. It is required in Rust."
                    )
                });
                let back = serde_json::to_value(&decoded)
                    .unwrap_or_else(|err| panic!("{here}: re-serialising failed: {err}"));
                // The reading half: absence has to mean exactly the declared
                // value, as it does for `Default`.
                let reread = back.get(&field.name).cloned().unwrap_or(Value::Null);
                assert!(
                    back.get(&field.name).is_none() || &reread == expected,
                    "{here}: the manifest says an absent key means `{expected}`, and the Rust \
                     type reads it as `{reread}`."
                );
                // The writing half, which is the whole of what this state adds:
                // a writer must *omit* the key when the value is the default,
                // so a decoder in another language has to apply the default to
                // real traffic and not only to hand-written requests.
                assert!(
                    back.get(&field.name).is_none(),
                    "{here} is declared optional-with-a-default, so a value equal to the default \
                     must be written as an absent key — the Rust type wrote `{reread}`. Either \
                     the manifest means plain `default`, or the Rust type is missing a \
                     `skip_serializing_if`."
                );
            }
        }
    }
}

/// One manifest struct against the Rust type that carries it.
///
/// `value` must have every field populated, including the optional ones —
/// their absent form is checked separately, by a caller that has a value to
/// show for it.
fn check_struct<T>(manifest: &Manifest, name: &str, value: &T)
where
    T: Serialize + DeserializeOwned,
{
    let types = manifest.type_map();
    let Some(TypeDef::Struct { fields, .. }) = types.get(name).copied() else {
        panic!("`{}` declares no struct named `{name}`", manifest.id());
    };
    check_object(name, fields, value, None);
}

/// That an optional field really is *omitted* rather than written as `null`,
/// which is the half of `optional` a decoder in another language depends on.
fn check_optional_is_omitted<T: Serialize>(
    manifest: &Manifest,
    name: &str,
    field: &str,
    value: &T,
) {
    let types = manifest.type_map();
    let Some(TypeDef::Struct { fields, .. }) = types.get(name).copied() else {
        panic!("`{}` declares no struct named `{name}`", manifest.id());
    };
    let declared = fields
        .iter()
        .find(|f| f.name == field)
        .unwrap_or_else(|| panic!("`{name}` declares no field `{field}`"));
    assert_eq!(
        declared.state(),
        FieldState::Optional,
        "`{name}.{field}` is no longer declared optional in the manifest"
    );
    let json = serde_json::to_value(value).expect("serialise");
    assert!(
        json.get(field).is_none(),
        "`{name}.{field}` is declared optional, so a value with nothing to say must omit the key \
         — the Rust type wrote `{}`",
        json.get(field).map_or(Value::Null, |v| v.clone())
    );
}

/// The union's tag and the exact set of arms it declares.
fn check_union_shape(manifest: &Manifest, name: &str, tag: &str, variants: &[&str]) {
    let types = manifest.type_map();
    let Some(TypeDef::Union {
        tagging,
        tag: declared_tag,
        variants: declared,
        ..
    }) = types.get(name).copied()
    else {
        panic!("`{}` declares no union named `{name}`", manifest.id());
    };
    assert_eq!(
        *tagging,
        Tagging::Internal,
        "`{name}` is no longer internally tagged in the manifest, and the Rust type is"
    );
    assert_eq!(
        declared_tag.as_deref(),
        Some(tag),
        "`{name}`'s discriminator key changed in the manifest"
    );
    let declared: Vec<&str> = declared.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(
        declared, variants,
        "`{name}`'s arms changed in the manifest; the Rust side is pinned to the second list"
    );
}

/// One arm of an internally tagged union against the Rust variant.
fn check_union_variant<T>(manifest: &Manifest, name: &str, variant: &str, value: &T)
where
    T: Serialize + DeserializeOwned,
{
    let types = manifest.type_map();
    let Some(TypeDef::Union {
        tag,
        variants: arms,
        ..
    }) = types.get(name).copied()
    else {
        panic!("`{}` declares no union named `{name}`", manifest.id());
    };
    let tag = tag
        .as_deref()
        .unwrap_or_else(|| panic!("`{name}` is internally tagged and names no tag"));
    let arm = arms
        .iter()
        .find(|v| v.name == variant)
        .unwrap_or_else(|| panic!("`{name}` declares no arm `{variant}`"));
    assert!(
        arm.payload.is_none(),
        "`{name}.{variant}` now carries a whole payload shape, which this test does not model"
    );

    let label = format!("{name}.{variant}");
    check_object(&label, &arm.fields, value, Some(tag));

    let json = serde_json::to_value(value).expect("serialise");
    assert_eq!(
        json.get(tag).and_then(Value::as_str),
        Some(arm.wire()),
        "{label}: the manifest says this arm travels as `{}` under `{tag}`, and the Rust value \
         wrote `{}`",
        arm.wire(),
        json.get(tag)
            .map_or_else(|| "nothing at all".to_string(), |v| v.to_string())
    );
}

/// A string enum: the exact set of values, and what each Rust variant writes.
fn check_enum<T: Serialize>(manifest: &Manifest, name: &str, values: &[(&str, T)]) {
    let types = manifest.type_map();
    let Some(TypeDef::Enum { variants, .. }) = types.get(name).copied() else {
        panic!("`{}` declares no enum named `{name}`", manifest.id());
    };
    let declared: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
    let pinned: Vec<&str> = values.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        declared, pinned,
        "`{name}`'s values changed in the manifest; the Rust side is pinned to the second list"
    );
    for (variant_name, value) in values {
        let declared = variants
            .iter()
            .find(|v| v.name == *variant_name)
            .expect("just checked");
        let json = serde_json::to_value(value).expect("serialise");
        assert_eq!(
            json.as_str(),
            Some(declared.wire()),
            "`{name}.{variant_name}`: the manifest says it travels as `{}`, and the Rust value \
             wrote `{json}`",
            declared.wire()
        );
    }
}

/// A newtype: that the manifest declares one, what it says it wraps, and —
/// the only part that is about bytes — that the Rust value **is** the inner
/// value rather than an envelope around it.
///
/// Transparency is the whole content of the newtype form, and it is the one
/// thing a round trip cannot check: a generator that invented a one-element
/// array for `HostId` would encode and decode its own invention perfectly.
/// So the wire form is asserted against a literal, in both directions.
fn check_newtype<T>(manifest: &Manifest, name: &str, inner: &str, value: &T, wire: &Value)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let types = manifest.type_map();
    let Some(TypeDef::Newtype { newtype, .. }) = types.get(name).copied() else {
        panic!(
            "`{}` declares no newtype named `{name}`. It is a transparent newtype in Rust, so a \
             declaration of `string` would be the format saying less than the Rust does.",
            manifest.id()
        );
    };
    let Schema::Named(keyword) = newtype else {
        panic!("`{name}` is declared over `{newtype:?}`, which is not a plain named shape");
    };
    assert_eq!(
        keyword, inner,
        "`{name}` is declared over `{keyword}` and the Rust type wraps a `{inner}`"
    );

    let encoded = serde_json::to_value(value).expect("a newtype serialises");
    assert_eq!(
        &encoded, wire,
        "`{name}` is declared a newtype, which is wire-identical to what it wraps — it must \
         encode as the bare `{inner}` and not as an object, an array or a tagged anything"
    );
    let decoded: T = serde_json::from_value(wire.clone())
        .unwrap_or_else(|err| panic!("`{name}` must decode from the bare `{inner}`: {err}"));
    assert_eq!(
        &decoded, value,
        "`{name}` decoded from its own wire form is a different value"
    );
}

/// The name a plain named schema carries, for comparing an op's declared
/// shapes against the pinned table.
fn schema_name(schema: Option<&Schema>) -> Option<&str> {
    match schema {
        None => None,
        Some(Schema::Named(name)) => Some(name.as_str()),
        Some(other) => panic!("expected a plain named schema, found `{other:?}`"),
    }
}

// -------------------------------------------------------------- the samples

fn contract_id() -> ContractId {
    ContractId::new("storage", 1)
}

fn endpoints() -> Endpoints {
    Endpoints {
        host: HostId::new("pl-0123456789abcdef0123456789abcdef"),
        ws: vec!["ws://127.0.0.1:9000/rpc".to_string()],
        uds: vec!["/tmp/storage.sock".to_string()],
        iroh: vec![IrohDial {
            ticket: "node-ticket".to_string(),
            alpn: "peerline/storage".to_string(),
        }],
    }
}

fn produced() -> Produced {
    Produced {
        contract: contract_id(),
        ops: vec!["storage.get".to_string()],
        cardinality: Cardinality::Single,
    }
}

fn consumed() -> Consumed {
    Consumed {
        contract: ContractId::new("index", 2),
        optional: true,
    }
}

fn binding() -> Binding {
    Binding {
        contract: contract_id(),
        instance: "storage-0".to_string(),
        endpoints: endpoints(),
    }
}

fn service_entry() -> ServiceEntry {
    ServiceEntry {
        service: "storage".to_string(),
        endpoints: endpoints(),
        produces: vec![produced()],
        consumes: vec![consumed()],
    }
}

fn service_endpoints() -> ServiceEndpoints {
    ServiceEndpoints {
        service: "storage".to_string(),
        endpoints: endpoints(),
    }
}

fn instance_info() -> InstanceInfo {
    InstanceInfo {
        instance: "storage-0".to_string(),
        pid: 4242,
        ready: true,
        last_seen_ms: 940,
        services: vec![service_entry()],
        waiting_on: vec![ContractId::new("index", 2)],
        unreachable: vec!["storage".to_string()],
    }
}

fn listing() -> Listing {
    Listing {
        ttl_ms: 30_000,
        address: "/tmp/peerline-coord.sock".to_string(),
        instances: vec![instance_info()],
    }
}

// ---------------------------------------------------------------- the tests

/// The format's own rules, including the cross-file ones: an import naming a
/// type the vocabulary does not declare, two contracts picking one error code,
/// a file whose name disagrees with what it declares.
#[test]
fn the_contracts_directory_loads_and_validates() {
    let dir = directory();
    let coord = manifest(&dir, COORD);
    let vocabulary = manifest(&dir, VOCABULARY);
    assert_eq!(coord.role, peerline_contract::spec::Role::Contract);
    assert_eq!(
        vocabulary.role,
        peerline_contract::spec::Role::Vocabulary,
        "`{VOCABULARY}/1` exists to be imported and declares no ops"
    );
    assert_eq!(
        coord.imports.len(),
        1,
        "`coord/1` takes its declaration shapes from exactly one vocabulary"
    );
    for name in &coord.imports[0].types {
        assert!(
            dir.resolve(coord, name).is_some(),
            "`coord/1` imports `{name}`, which does not resolve in the directory"
        );
    }
}

#[test]
fn every_op_is_declared_with_the_kind_and_the_shapes_the_rust_side_uses() {
    // What a caller gets back: a unary op's result, or a stream's item. Named
    // rather than two `Option`s so that the pairing with `kind` is one value
    // and cannot be written inconsistently.
    enum Answer {
        Reply(&'static str),
        Item(&'static str),
    }
    use Answer::{Item, Reply};

    // Lifecycle order, which is the order the manifest lists them in and the
    // order a joiner calls them in.
    let table: &[(&str, &str, OpKind, Option<&str>, Answer)] = &[
        (
            "OP_REGISTER",
            coord::OP_REGISTER,
            OpKind::Unary,
            Some("RegisterArgs"),
            Reply("RegisterAck"),
        ),
        (
            "OP_AWAIT_READY",
            coord::OP_AWAIT_READY,
            OpKind::Stream,
            Some("AwaitReadyArgs"),
            Item("Binding"),
        ),
        (
            "OP_READY",
            coord::OP_READY,
            OpKind::Unary,
            Some("ReadyArgs"),
            Reply("ReadyResult"),
        ),
        (
            "OP_HEARTBEAT",
            coord::OP_HEARTBEAT,
            OpKind::Unary,
            Some("Heartbeat"),
            Reply("HeartbeatResult"),
        ),
        (
            "OP_UPDATE",
            coord::OP_UPDATE,
            OpKind::Unary,
            Some("UpdateArgs"),
            Reply("UpdateAck"),
        ),
        (
            "OP_RESOLVE",
            coord::OP_RESOLVE,
            OpKind::Unary,
            Some("ResolveArgs"),
            Reply("ResolveResult"),
        ),
        (
            "OP_LIST",
            coord::OP_LIST,
            OpKind::Unary,
            None,
            Reply("Listing"),
        ),
        (
            "OP_BINDINGS",
            coord::OP_BINDINGS,
            OpKind::Unary,
            None,
            Reply("BindingGraph"),
        ),
        (
            "OP_WATCH",
            coord::OP_WATCH,
            OpKind::Stream,
            None,
            Item("WatchEvent"),
        ),
    ];

    let dir = directory();
    let coord = manifest(&dir, COORD);
    let ops = coord.op_map();

    let declared: BTreeSet<&str> = ops.keys().copied().collect();
    let pinned: BTreeSet<&str> = table.iter().map(|row| row.1).collect();
    assert_eq!(
        declared, pinned,
        "the manifest's ops and this crate's `OP_*` constants name different sets"
    );

    for (konst, name, kind, args, answer) in table {
        let op = ops.get(name).expect("just checked the sets match");
        assert_eq!(op.kind, *kind, "op `{name}`: the manifest's kind changed");
        assert_eq!(
            const_name("OP", op.short_name(COORD)),
            *konst,
            "op `{name}` would generate a constant that is not the one this crate declares"
        );
        assert_eq!(
            schema_name(op.args.as_ref()),
            *args,
            "op `{name}`: the manifest's args changed"
        );
        match answer {
            Reply(ty) => {
                assert_eq!(
                    schema_name(op.result.as_ref()),
                    Some(*ty),
                    "op `{name}`: the manifest's result changed"
                );
                assert!(
                    op.item.is_none(),
                    "op `{name}` is unary and declares an item"
                );
            }
            Item(ty) => {
                assert_eq!(
                    schema_name(op.item.as_ref()),
                    Some(*ty),
                    "op `{name}`: the manifest's stream item changed"
                );
                assert!(
                    op.result.is_none(),
                    "op `{name}` is a stream and declares a result"
                );
            }
        }
    }
}

#[test]
fn every_error_code_and_its_data_shape_match_the_constants() {
    /// One error, from both sides: the constant this crate declares and the
    /// name the manifest declares, plus the ops that must still say they can
    /// raise it.
    struct Row {
        konst: &'static str,
        code: i32,
        name: &'static str,
        data: Option<&'static str>,
        raised_by: &'static [&'static str],
    }

    let table = &[
        Row {
            konst: "ERR_INSTANCE_EXISTS",
            code: coord::ERR_INSTANCE_EXISTS,
            name: "instance_exists",
            data: None,
            raised_by: &["coord.register"],
        },
        Row {
            konst: "ERR_UNKNOWN_INSTANCE",
            code: coord::ERR_UNKNOWN_INSTANCE,
            name: "unknown_instance",
            data: None,
            // Two ops name an instance the coordinator is expected to already
            // hold. `ready` and `heartbeat` answer a value instead, which is
            // why they are not here — see the error's prose.
            raised_by: &["coord.await_ready", "coord.update"],
        },
        Row {
            konst: "ERR_UNRESOLVED",
            code: coord::ERR_UNRESOLVED,
            name: "unresolved",
            data: Some("Unresolved"),
            raised_by: &["coord.await_ready"],
        },
        Row {
            konst: "ERR_SINGLETON_OCCUPIED",
            code: coord::ERR_SINGLETON_OCCUPIED,
            name: "singleton_occupied",
            data: Some("SingletonOccupied"),
            raised_by: &["coord.register"],
        },
    ];

    let dir = directory();
    let coord = manifest(&dir, COORD);
    let errors = coord.error_map();

    let declared: BTreeSet<&str> = errors.keys().copied().collect();
    let pinned: BTreeSet<&str> = table.iter().map(|row| row.name).collect();
    assert_eq!(
        declared, pinned,
        "the manifest's errors and this crate's `ERR_*` constants name different sets"
    );

    for Row {
        konst,
        code,
        name,
        data,
        raised_by,
    } in table
    {
        let err = errors.get(name).expect("just checked the sets match");
        assert_eq!(
            err.code, *code,
            "error `{name}`: the manifest's code and `{konst}` disagree"
        );
        assert_eq!(
            err.const_name(),
            *konst,
            "error `{name}` would generate a constant that is not the one this crate declares"
        );
        assert_eq!(
            schema_name(err.data.as_ref()),
            *data,
            "error `{name}`: the manifest's data shape changed"
        );
        for op_name in *raised_by {
            let op = coord.op_map();
            let op = op.get(op_name).expect("a declared op");
            assert!(
                op.errors.iter().any(|e| e == name),
                "op `{op_name}` no longer declares that it can raise `{name}`"
            );
        }
    }
}

#[test]
fn the_vocabulary_shapes_match_the_hand_written_types() {
    let dir = directory();
    let m = manifest(&dir, VOCABULARY);

    check_struct(m, "ContractId", &contract_id());
    check_struct(m, "Produced", &produced());
    check_struct(m, "Consumed", &consumed());
    check_struct(m, "IrohDial", &endpoints().iroh[0]);
    check_struct(m, "Endpoints", &endpoints());
    check_struct(m, "Binding", &binding());

    // `HostId` is a `#[serde(transparent)]` newtype over `String` in Rust, and
    // the manifest says so as of the format's newtype form. It used to declare
    // `Endpoints.host` as a bare `string`, with the fact that Rust wraps it
    // listed in `contracts/README.md` under things the format could not say —
    // which meant the one property the wrapper must have, that it adds nothing
    // to the bytes, was asserted nowhere.
    check_newtype(
        m,
        "HostId",
        "string",
        &HostId::new("pl-0123456789abcdef0123456789abcdef"),
        &Value::String("pl-0123456789abcdef0123456789abcdef".to_string()),
    );

    check_enum(
        m,
        "Cardinality",
        &[("any", Cardinality::Any), ("single", Cardinality::Single)],
    );
}

#[test]
fn the_coord_shapes_match_the_hand_written_types() {
    let dir = directory();
    let m = manifest(&dir, COORD);

    check_struct(m, "ServiceEntry", &service_entry());
    check_struct(
        m,
        "RegisterArgs",
        &RegisterArgs {
            instance: "storage-0".to_string(),
            pid: 4242,
            services: vec![service_entry()],
        },
    );
    check_struct(
        m,
        "RegisterAck",
        &RegisterAck {
            id: "storage-0".to_string(),
        },
    );
    check_struct(
        m,
        "AwaitReadyArgs",
        &AwaitReadyArgs {
            instance: "storage-0".to_string(),
            timeout_ms: 30_000,
        },
    );
    check_struct(
        m,
        "Unresolved",
        &Unresolved {
            instance: "fs-0".to_string(),
            waited_ms: 30_000,
            unresolved: vec![contract_id()],
            cycle: vec![contract_id(), ContractId::new("index", 2), contract_id()],
        },
    );
    check_struct(
        m,
        "ReadyArgs",
        &ReadyArgs {
            instance: "storage-0".to_string(),
        },
    );
    check_struct(
        m,
        "Heartbeat",
        &Heartbeat {
            instance: "storage-0".to_string(),
        },
    );
    check_struct(m, "ServiceEndpoints", &service_endpoints());
    check_struct(
        m,
        "UpdateArgs",
        &UpdateArgs {
            instance: "storage-0".to_string(),
            services: vec![service_endpoints()],
        },
    );
    check_struct(
        m,
        "UpdateAck",
        &UpdateAck {
            id: "storage-0".to_string(),
        },
    );
    check_struct(m, "ResolveArgs", &ResolveArgs::by("fs-0", contract_id()));
    check_optional_is_omitted(
        m,
        "ResolveArgs",
        "instance",
        &ResolveArgs::new(contract_id()),
    );
    check_struct(
        m,
        "ResolveResult",
        &ResolveResult {
            bindings: vec![binding()],
        },
    );
    check_struct(
        m,
        "SingletonOccupied",
        &SingletonOccupied {
            contract: contract_id(),
            instance: "storage-1".to_string(),
            holder: "storage-0".to_string(),
            disagreed: true,
        },
    );
    check_struct(m, "InstanceInfo", &instance_info());
    check_struct(m, "Listing", &listing());
    check_struct(
        m,
        "Edge",
        &Edge {
            consumer: "fs-0".to_string(),
            contract: contract_id(),
            producer: "storage-0".to_string(),
            age_ms: 1_500,
        },
    );
    check_struct(
        m,
        "BindingGraph",
        &BindingGraph {
            edges: vec![Edge {
                consumer: "fs-0".to_string(),
                contract: contract_id(),
                producer: "storage-0".to_string(),
                age_ms: 1_500,
            }],
        },
    );
}

#[test]
fn the_internally_tagged_unions_are_tagged_and_spelled_the_same_way() {
    let dir = directory();
    let m = manifest(&dir, COORD);

    check_union_shape(m, "ReadyResult", "status", &["ok", "unknown_instance"]);
    check_union_variant(m, "ReadyResult", "ok", &ReadyResult::Ok);
    check_union_variant(
        m,
        "ReadyResult",
        "unknown_instance",
        &ReadyResult::UnknownInstance,
    );

    check_union_shape(m, "HeartbeatResult", "status", &["ok", "unknown_instance"]);
    check_union_variant(m, "HeartbeatResult", "ok", &HeartbeatResult::Ok);
    check_union_variant(
        m,
        "HeartbeatResult",
        "unknown_instance",
        &HeartbeatResult::UnknownInstance,
    );

    check_union_shape(m, "WatchEvent", "kind", &["snapshot", "upsert", "remove"]);
    check_union_variant(
        m,
        "WatchEvent",
        "snapshot",
        &WatchEvent::Snapshot {
            registry: listing(),
        },
    );
    check_union_variant(
        m,
        "WatchEvent",
        "upsert",
        &WatchEvent::Upsert {
            instance: instance_info(),
        },
    );
    check_union_variant(
        m,
        "WatchEvent",
        "remove",
        &WatchEvent::Remove {
            instance: "storage-0".to_string(),
        },
    );
}

/// The two results have the same wire form today, which is what lets a caller
/// handle both with one branch — and they are separate types so that a future
/// arm on one is not an unannounced arm on the other. Both halves of that are
/// worth pinning, because the first is what an implementor relies on and the
/// second is what stops it being relied on too hard.
#[test]
fn ready_and_heartbeat_answer_the_same_shape_from_two_declarations() {
    let dir = directory();
    let m = manifest(&dir, COORD);
    let types = m.type_map();
    let arms = |name: &str| match types.get(name).copied() {
        Some(TypeDef::Union { tag, variants, .. }) => (
            tag.clone(),
            variants
                .iter()
                .map(|v| v.wire().to_string())
                .collect::<Vec<_>>(),
        ),
        _ => panic!("`{name}` is not a union"),
    };
    assert_eq!(
        arms("ReadyResult"),
        arms("HeartbeatResult"),
        "the two results no longer share a wire form; a caller handling both with one branch is \
         now wrong, and the manifests' prose says otherwise"
    );
    assert_eq!(
        serde_json::to_value(ReadyResult::UnknownInstance).expect("serialise"),
        serde_json::to_value(HeartbeatResult::UnknownInstance).expect("serialise"),
        "and the Rust types no longer agree either"
    );
}
