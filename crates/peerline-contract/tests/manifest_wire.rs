//! The manifest envelope's wire form. It crosses a process boundary — one
//! binary writes it, a supervisor reads it — so the field names are the
//! contract, exactly as for the `coord.*` shapes.

use peerline_contract::manifest::{MANIFEST_OUT_ENV, Manifest, ServiceManifest};
use peerline_contract::{Consumed, ContractId, Produced};

#[test]
fn the_envelope_is_a_list_of_services() {
    // One process may mount several services, each with its own contracts, so
    // the file is keyed on `services` rather than being one service's object.
    let manifest = Manifest::new()
        .with(ServiceManifest::new("fs").producing(Produced::with_ops("fs", 1, ["fs.read"])))
        .with(ServiceManifest::new("index").consuming(Consumed::new("storage", 1)));
    let json: serde_json::Value =
        serde_json::from_str(&manifest.to_json().unwrap()).expect("valid json");

    assert_eq!(json["services"][0]["service"], "fs");
    assert_eq!(json["services"][0]["produces"][0]["contract"]["name"], "fs");
    assert_eq!(json["services"][0]["produces"][0]["contract"]["version"], 1);
    assert_eq!(json["services"][0]["produces"][0]["ops"][0], "fs.read");
    assert_eq!(json["services"][1]["service"], "index");
    assert_eq!(
        json["services"][1]["consumes"][0]["contract"]["name"],
        "storage"
    );
    assert_eq!(json["services"][1]["consumes"][0]["optional"], false);
}

#[test]
fn produces_and_consumes_may_be_omitted() {
    // A binary that declares nothing writes the minimum. Requiring the empty
    // arrays would make the writing side's job needlessly fussy.
    let manifest = Manifest::from_json(br#"{"services":[{"service":"coord"}]}"#).expect("parses");
    assert_eq!(manifest, Manifest::single(ServiceManifest::new("coord")));
    assert_eq!(
        Manifest::from_json(b"{}").expect("parses"),
        Manifest::new(),
        "a process that mounts nothing is a manifest with no services"
    );
}

#[test]
fn optional_defaults_to_required() {
    let manifest = Manifest::from_json(
        br#"{"services":[{"service":"fs",
             "consumes":[{"contract":{"name":"storage","version":1}}]}]}"#,
    )
    .expect("parses");
    let consumed = &manifest.services[0].consumes[0];
    assert!(!consumed.optional, "omitted means required");
    assert_eq!(consumed.contract, ContractId::new("storage", 1));
}

#[test]
fn unknown_fields_are_ignored() {
    // Contract shapes are additive across versions, so a newer binary reporting
    // a field this supervisor has never heard of must still be usable rather
    // than fail the probe. (A cluster *config* takes the opposite stance.)
    let manifest =
        Manifest::from_json(br#"{"services":[{"service":"fs","future":[1,2]}],"extra":true}"#)
            .expect("parses");
    assert_eq!(manifest.services[0].service, "fs");
}

#[test]
fn round_trips_and_flattens() {
    let manifest = Manifest::new()
        .with(ServiceManifest::new("indexer").producing(Produced::new("index", 1)))
        .with(ServiceManifest::new("cache").consuming(Consumed::optional("metrics", 2)));
    let text = manifest.to_json().unwrap();
    assert_eq!(Manifest::from_json(text.as_bytes()).unwrap(), manifest);

    // The flattened views are what a graph is built from: readiness is per
    // process, so the union across mounted services is the honest unit.
    assert_eq!(manifest.produces().count(), 1);
    assert_eq!(manifest.consumes().count(), 1);
}

#[test]
fn the_variable_name_is_the_one_both_sides_agree_on() {
    assert_eq!(MANIFEST_OUT_ENV, "PEERLINE_MANIFEST_OUT");
}
