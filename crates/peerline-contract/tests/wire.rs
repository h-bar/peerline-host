//! The wire form is a cross-language contract: a TypeScript implementation has
//! to be able to produce and read exactly these objects. So each test asserts
//! the literal JSON — field names included — rather than only that a value
//! survives a round trip through itself.

use peerline_contract::coord::ServiceEntry;
use peerline_contract::{
    Binding, Cardinality, Consumed, ContractId, Endpoints, HostId, IrohDial, Produced,
};
use serde_json::{Value, json};

/// Serializes to `expect`, and `expect` deserializes back to the original.
fn round_trip<T>(value: &T, expect: Value)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(value).expect("serialize");
    assert_eq!(encoded, expect, "wire form changed");
    let decoded: T = serde_json::from_value(expect).expect("deserialize");
    assert_eq!(&decoded, value, "round trip changed the value");
}

#[test]
fn contract_id_wire_form() {
    round_trip(
        &ContractId::new("storage", 1),
        json!({ "name": "storage", "version": 1 }),
    );
}

#[test]
fn contract_id_displays_as_name_slash_version() {
    assert_eq!(ContractId::new("storage", 1).to_string(), "storage/1");
}

#[test]
fn contract_id_version_is_an_integer_not_a_semver_string() {
    let encoded = serde_json::to_value(ContractId::new("fs", 2)).unwrap();
    assert!(
        encoded["version"].is_u64(),
        "version must be a JSON number: {encoded}"
    );
    assert!(serde_json::from_value::<ContractId>(json!({ "name": "fs", "version": "2" })).is_err());
}

#[test]
fn produced_wire_form() {
    round_trip(
        &Produced::with_ops("storage", 1, ["storage.get", "storage.put"]),
        json!({
            "contract": { "name": "storage", "version": 1 },
            "ops": ["storage.get", "storage.put"],
            "cardinality": "any",
        }),
    );
    round_trip(
        &Produced::single("storage", 1),
        json!({
            "contract": { "name": "storage", "version": 1 },
            "ops": [],
            "cardinality": "single",
        }),
    );
}

#[test]
fn produced_ops_default_to_empty() {
    let decoded: Produced =
        serde_json::from_value(json!({ "contract": { "name": "fs", "version": 1 } })).unwrap();
    assert_eq!(decoded, Produced::new("fs", 1));
    assert!(decoded.ops.is_empty());
}

/// The wire default, and the whole reason the field could be added at all:
/// every registration written before `cardinality` existed — and every one
/// written by an implementation in another language that has never heard of it
/// — must keep deserialising, and must keep meaning "any number of
/// interchangeable producers". Additive within a contract version is the
/// discipline; a field that changed the meaning of an existing message would
/// not be additive.
#[test]
fn produced_cardinality_defaults_to_any() {
    let decoded: Produced = serde_json::from_value(json!({
        "contract": { "name": "storage", "version": 1 },
        "ops": ["storage.get"],
    }))
    .unwrap();
    assert_eq!(decoded.cardinality, Cardinality::Any);
    assert_eq!(decoded, Produced::with_ops("storage", 1, ["storage.get"]));

    // And it survives the whole envelope it actually travels in, not just the
    // leaf: a `coord.register` recorded before the field existed.
    let entry: ServiceEntry = serde_json::from_value(json!({
        "service": "storage",
        "endpoints": { "host": "h", "uds": ["/tmp/s.sock"] },
        "produces": [{ "contract": { "name": "storage", "version": 1 } }],
    }))
    .unwrap();
    assert_eq!(entry.produces[0].cardinality, Cardinality::Any);
}

/// The claim reads as a claim, and it is spelled the same in every language:
/// lower-case, not `"Single"`.
#[test]
fn cardinality_wire_form() {
    round_trip(&Cardinality::Any, json!("any"));
    round_trip(&Cardinality::Single, json!("single"));
    assert_eq!(Cardinality::default(), Cardinality::Any);
    assert!(Produced::single("storage", 1).cardinality.is_single());
    assert!(!Produced::new("storage", 1).cardinality.is_single());
    assert!(
        Produced::with_ops("storage", 1, ["storage.get"])
            .with_cardinality(Cardinality::Single)
            .cardinality
            .is_single()
    );
}

#[test]
fn consumed_wire_form() {
    round_trip(
        &Consumed::new("storage", 1),
        json!({ "contract": { "name": "storage", "version": 1 }, "optional": false }),
    );
    round_trip(
        &Consumed::optional("index", 3),
        json!({ "contract": { "name": "index", "version": 3 }, "optional": true }),
    );
}

#[test]
fn consumed_defaults_to_required() {
    let decoded: Consumed =
        serde_json::from_value(json!({ "contract": { "name": "storage", "version": 1 } })).unwrap();
    assert!(
        !decoded.optional,
        "an omitted `optional` must read as required"
    );
}

#[test]
fn host_id_is_a_bare_string_on_the_wire() {
    // Transparent: no `{ "0": ... }` wrapper for another language to reproduce.
    round_trip(&HostId::new("host-a"), json!("host-a"));
}

#[test]
fn iroh_dial_wire_form() {
    round_trip(
        &IrohDial {
            ticket: "node1abc".into(),
            alpn: "peerline/storage".into(),
        },
        json!({ "ticket": "node1abc", "alpn": "peerline/storage" }),
    );
}

#[test]
fn endpoints_wire_form_carries_the_locality_tag() {
    let mut endpoints = Endpoints::new(HostId::new("host-a"));
    endpoints.ws.push("ws://10.0.0.2:9000/storage".into());
    endpoints.uds.push("/tmp/storage.sock".into());
    endpoints.iroh.push(IrohDial {
        ticket: "node1abc".into(),
        alpn: "peerline/storage".into(),
    });
    round_trip(
        &endpoints,
        json!({
            "host": "host-a",
            "ws": ["ws://10.0.0.2:9000/storage"],
            "uds": ["/tmp/storage.sock"],
            "iroh": [{ "ticket": "node1abc", "alpn": "peerline/storage" }],
        }),
    );
}

#[test]
fn endpoints_transport_lists_default_to_empty_but_host_is_required() {
    let decoded: Endpoints = serde_json::from_value(json!({ "host": "host-a" })).unwrap();
    assert_eq!(decoded, Endpoints::new(HostId::new("host-a")));

    // No locality tag means no `Endpoints` at all — the field cannot be
    // silently defaulted, or "unknown host" would compare equal to "same host".
    let err = serde_json::from_value::<Endpoints>(json!({ "uds": ["/tmp/storage.sock"] }));
    assert!(err.is_err(), "`host` must be mandatory");
}

#[test]
fn uds_is_usable_only_from_the_reporting_host() {
    let endpoints = Endpoints::new(HostId::new("host-a"));
    assert!(endpoints.uds_usable_from(&HostId::new("host-a")));
    assert!(!endpoints.uds_usable_from(&HostId::new("host-b")));
}

#[test]
fn binding_wire_form() {
    let mut endpoints = Endpoints::new(HostId::new("host-a"));
    endpoints.uds.push("/tmp/storage.sock".into());
    round_trip(
        &Binding {
            contract: ContractId::new("storage", 1),
            instance: "storage@/tmp/storage.sock".into(),
            endpoints,
        },
        json!({
            "contract": { "name": "storage", "version": 1 },
            "instance": "storage@/tmp/storage.sock",
            "endpoints": { "host": "host-a", "ws": [], "uds": ["/tmp/storage.sock"], "iroh": [] },
        }),
    );
}

#[test]
fn unknown_fields_are_ignored_because_a_version_is_additive() {
    // A newer peer adding a field within the same contract major must not break
    // an older one; that is what "additive within a version" buys.
    let decoded: Consumed = serde_json::from_value(json!({
        "contract": { "name": "storage", "version": 1, "channel": "beta" },
        "optional": true,
        "deadline_ms": 30000,
    }))
    .unwrap();
    assert_eq!(decoded, Consumed::optional("storage", 1));
}
