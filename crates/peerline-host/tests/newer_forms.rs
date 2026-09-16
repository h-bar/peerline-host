//! Maps, newtypes and the absent-able default, over a real connection.
//!
//! # What this is for
//!
//! The macro crate's token tests can read what the expansion *says*: that the
//! field is a `BTreeMap`, that the struct carries `#[serde(transparent)]`,
//! that the `skip_serializing_if` names a predicate which compares against the
//! getter. None of that is the claim. Each of these three forms makes a claim
//! about **bytes**, and a claim about bytes is only checkable with bytes:
//!
//! - a **map** is a JSON object, keyed by the map's own keys and by nothing
//!   the generator added;
//! - a **newtype** is wire-identical to what it wraps — `HostId` is the
//!   string, not a one-element array and not an object with a field in it;
//! - a field that is `optional` **and** `default` is genuinely *missing* from
//!   the object when it holds the default, and genuinely *present* when it
//!   does not, and means the default when it is missing on the way in.
//!
//! So every one of those is asserted against a literal first — because "both
//! ends agree" is also true when both ends are wrong in the same way — and
//! then run across a loopback connection, in a unary result, in a bare result
//! position, and as a stream item, which are three different encoders.
//!
//! # Why the fixture is its own directory
//!
//! `tests/contracts` and `tests/contracts-inline` belong to tests that were
//! here first, and a manifest added to either of them is a manifest every one
//! of those tests loads: `Directory::load` reads the whole directory and
//! checks it as a unit. `tests/contracts-forms` holds one file that nothing
//! else reads, so this suite cannot make another one fail by existing.

use std::sync::Arc;

use futures::StreamExt;
use peerline::runtime::{Peer, loopback};
use peerline::wire::RpcError;

mod forms {
    //! The contract, expanded with the runtime stubs so both ends of the
    //! connection below are generated rather than written.
    peerline_contract::contract!("demo.forms/1" in "tests/contracts-forms", runtime);
}

// ---------------------------------------------------------------------------
// The values
// ---------------------------------------------------------------------------

/// A report with every field at something other than its default, so that
/// nothing below passes because two absences happened to line up.
fn full_report() -> forms::Report {
    let mut report = forms::Report::new(
        forms::HostId::new("host-1"),
        std::collections::BTreeMap::from([
            ("HOME".to_string(), "/root".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ]),
    );
    report.counts = std::collections::BTreeMap::from([("retries".to_string(), 2)]);
    report.tags = vec!["slow".to_string()];
    report.label = "the first one".to_string();
    report.took = forms::Millis::new(1_500);
    report.note = Some("it took a while".to_string());
    report
}

/// The same report with every absent-able field left where the manifest puts
/// it, which is the value whose encoding is mostly *missing keys*.
fn bare_report() -> forms::Report {
    forms::Report::new(
        forms::HostId::new("host-1"),
        std::collections::BTreeMap::from([("HOME".to_string(), "/root".to_string())]),
    )
}

// ---------------------------------------------------------------------------
// The wire form, against a literal
// ---------------------------------------------------------------------------

/// **A map is a JSON object and a newtype is the value it wraps.** Asserted
/// against the literal the manifest describes, because the round trips below
/// would pass just as well if the generator had invented an envelope for
/// `HostId` — both ends would invent the same one.
#[test]
fn the_new_forms_encode_as_the_manifest_says_they_do() {
    let wire = serde_json::to_value(full_report()).expect("a generated shape serialises");
    assert_eq!(
        wire,
        serde_json::json!({
            "host": "host-1",
            "env": { "HOME": "/root", "PATH": "/usr/bin" },
            "counts": { "retries": 2 },
            "tags": ["slow"],
            "label": "the first one",
            "took": 1_500_i64,
            "note": "it took a while"
        }),
        "a newtype adds nothing to the wire and a map is the object it is",
    );
}

/// A newtype on its own, in the position where an envelope would be most
/// visible: there is no surrounding object for one to hide in.
#[test]
fn a_newtype_alone_is_the_value_it_wraps() {
    assert_eq!(
        serde_json::to_value(forms::HostId::new("host-1")).expect("serialises"),
        serde_json::json!("host-1"),
    );
    assert_eq!(
        serde_json::to_value(forms::Millis::new(1_500)).expect("serialises"),
        serde_json::json!(1_500_i64),
    );
    // And back, from exactly those bytes.
    let host: forms::HostId = serde_json::from_value(serde_json::json!("host-1")).expect("decodes");
    assert_eq!(host, forms::HostId::new("host-1"));
    assert_eq!(host.get(), "host-1");
    assert_eq!(host.into_inner(), "host-1");
    // The conversions the name is worth having for.
    let from: forms::HostId = "host-2".to_string().into();
    let back: String = from.clone().into();
    assert_eq!(back, "host-2");
}

/// **The half of the pair no earlier field state could express.** A field
/// holding its declared default is *absent from the object*, not written as
/// its value — which is what `skip_serializing_if` buys and what the format
/// previously had to describe in prose.
#[test]
fn a_defaulted_absent_able_key_is_missing_when_it_holds_the_default() {
    let wire = serde_json::to_value(bare_report()).expect("serialises");
    assert_eq!(
        wire,
        serde_json::json!({ "host": "host-1", "env": { "HOME": "/root" } }),
        "every absent-able key is gone, including the four with defaults",
    );

    let object = wire.as_object().expect("an object");
    for key in ["counts", "tags", "label", "took", "note"] {
        assert!(
            !object.contains_key(key),
            "`{key}` should be absent: {object:?}"
        );
    }
}

/// And the other half: a field holding anything *else* is written. A
/// predicate that skipped unconditionally would pass the test above and fail
/// here, which is why both are asserted rather than only the interesting one.
#[test]
fn a_defaulted_absent_able_key_is_written_when_it_holds_anything_else() {
    let mut report = bare_report();
    report.took = forms::Millis::new(1);
    report.counts = std::collections::BTreeMap::from([("a".to_string(), 0)]);

    let wire = serde_json::to_value(report).expect("serialises");
    let object = wire.as_object().expect("an object");
    assert_eq!(object.get("took"), Some(&serde_json::json!(1_i64)));
    assert_eq!(object.get("counts"), Some(&serde_json::json!({ "a": 0 })));
    assert!(
        !object.contains_key("label"),
        "the ones still at their default stay absent: {object:?}",
    );
}

/// On the way in, an absent key means the declared default — and the declared
/// default is not always the Rust type's. `label` is `"unnamed"` and not the
/// empty string, so a generator that derived `Default` would fail here.
#[test]
fn an_absent_key_means_the_manifests_default_and_not_rusts() {
    let report: forms::Report =
        serde_json::from_value(serde_json::json!({ "host": "h", "env": {} }))
            .expect("every other key is absent-able");

    assert_eq!(report.label, "unnamed");
    assert_eq!(report.took, forms::Millis::new(0));
    assert!(report.counts.is_empty());
    assert!(report.tags.is_empty());
    // The one absent-able field with no default is the contrast: absence
    // there means nothing was said, and there is no value to apply.
    assert_eq!(report.note, None);
}

/// A decoder that tolerates less than it emits helps nobody, so the key may
/// also arrive explicitly — including holding the very value that would have
/// been omitted.
#[test]
fn the_key_may_also_arrive_explicitly_even_holding_the_default() {
    let report: forms::Report = serde_json::from_value(serde_json::json!({
        "host": "host-1",
        "env": { "HOME": "/root" },
        "label": "unnamed",
        "took": 0_i64,
        "counts": {},
        "tags": []
    }))
    .expect("decodes");
    assert_eq!(
        report,
        bare_report(),
        "the explicit form and the omitted form decode to one value",
    );
}

/// The map's keys are the contract's data and nothing normalises them: a key
/// that is not an identifier in anybody's language is a key, because a JSON
/// object's keys are strings.
#[test]
fn a_maps_keys_are_whatever_strings_the_value_holds() {
    let mut report = bare_report();
    report.env = std::collections::BTreeMap::from([
        ("kebab-case".to_string(), "1".to_string()),
        ("with space".to_string(), "2".to_string()),
        (String::new(), "3".to_string()),
    ]);
    let wire = serde_json::to_value(&report).expect("serialises");
    assert_eq!(
        wire["env"],
        serde_json::json!({ "": "3", "kebab-case": "1", "with space": "2" }),
    );
    let back: forms::Report = serde_json::from_value(wire).expect("decodes");
    assert_eq!(back, report);
}

/// The ordered map is not a preference. A wire type is serialised, and an
/// unordered one would give one value two encodings — which every byte
/// comparison downstream depends on not happening.
#[test]
fn a_maps_encoding_is_the_same_bytes_whatever_order_it_was_built_in() {
    let entries = [("b", "2"), ("a", "1"), ("c", "3")];
    let forwards: std::collections::BTreeMap<String, String> = entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let backwards: std::collections::BTreeMap<String, String> = entries
        .iter()
        .rev()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();

    let mut one = bare_report();
    one.env = forwards;
    let mut other = bare_report();
    other.env = backwards;

    assert_eq!(
        serde_json::to_string(&one).expect("serialises"),
        serde_json::to_string(&other).expect("serialises"),
        "one value, one encoding, on every machine and every run",
    );
    assert!(
        serde_json::to_string(&one)
            .expect("serialises")
            .contains(r#""env":{"a":"1","b":"2","c":"3"}"#),
        "and the order is the keys' own",
    );
}

// ---------------------------------------------------------------------------
// Over a connection
// ---------------------------------------------------------------------------

/// A producer that hands back whatever arrived, so that a difference between
/// the two ends is a difference in the value rather than in this.
struct Echo;

impl forms::DemoFormsService for Echo {
    async fn round(&self, args: forms::Report) -> Result<forms::Report, RpcError> {
        Ok(args)
    }

    async fn host(&self, args: forms::Report) -> Result<forms::HostId, RpcError> {
        Ok(args.host)
    }

    async fn env_read(
        &self,
        args: forms::Report,
    ) -> Result<std::collections::BTreeMap<String, String>, RpcError> {
        Ok(args.env)
    }

    async fn watch(&self, args: forms::Report, items: forms::WatchItems) -> Result<(), RpcError> {
        for _ in 0..3 {
            if items.send(&args).is_err() {
                return Ok(());
            }
        }
        Ok(())
    }
}

fn connected() -> (forms::DemoFormsClient, Peer) {
    let (consumer, producer, driver) = loopback();
    tokio::spawn(driver);
    forms::register_demo_forms_handlers(&producer, Arc::new(Echo));
    (forms::DemoFormsClient::new(consumer), producer)
}

/// The whole checklist across a real connection: a value with every field set
/// goes out, is decoded by a producer generated from the same manifest, and
/// comes back equal.
#[tokio::test]
async fn a_full_report_survives_a_round_trip() {
    let (client, _producer) = connected();
    let sent = full_report();
    let back = client.round(&sent).await.expect("the producer answered");
    assert_eq!(back, sent);
}

/// **The one that only the pair makes interesting.** The bare report encodes
/// to two keys; everything else is absent on the wire and is reconstructed at
/// the far end from the manifest's declared defaults. A producer that filled
/// them from Rust's own defaults would answer with `label: ""`.
#[tokio::test]
async fn a_report_whose_defaults_never_reach_the_wire_arrives_intact() {
    let (client, _producer) = connected();
    let sent = bare_report();
    let back = client.round(&sent).await.expect("the producer answered");
    assert_eq!(back, sent);
    assert_eq!(back.label, "unnamed");
    assert_eq!(back.took, forms::Millis::new(0));
}

/// A newtype in a bare result position, where there is no surrounding object.
#[tokio::test]
async fn a_newtype_crosses_the_connection_as_a_bare_result() {
    let (client, _producer) = connected();
    let host = client
        .host(&full_report())
        .await
        .expect("the producer answered");
    assert_eq!(host, forms::HostId::new("host-1"));
}

/// A map in a bare result position, likewise.
#[tokio::test]
async fn a_map_crosses_the_connection_as_a_bare_result() {
    let (client, _producer) = connected();
    let env = client
        .env_read(&full_report())
        .await
        .expect("the producer answered");
    assert_eq!(
        env,
        std::collections::BTreeMap::from([
            ("HOME".to_string(), "/root".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ]),
    );
}

/// A stream item is encoded by the generated sink rather than by the unary
/// result path, so a form that was right for one and wrong for the other
/// would pass everything above.
#[tokio::test]
async fn the_new_forms_cross_as_stream_items_too() {
    let (client, _producer) = connected();
    let mut items = client.watch(&bare_report()).expect("the stream opened");

    let mut seen = Vec::new();
    while let Some(item) = items.next().await {
        seen.push(item.expect("an item, not a fault").data);
    }
    assert_eq!(seen, vec![bare_report(); 3]);
}

// ---------------------------------------------------------------------------
// The constants the expansion carries beside the shapes
// ---------------------------------------------------------------------------

/// The error code is inside the band this contract claims, which is the whole
/// of what claiming one asserts — and the band is checked by the loader, so a
/// code outside it never reaches an expansion at all.
#[test]
fn the_declared_error_code_is_inside_the_contracts_band() {
    assert_eq!(forms::ERR_NO_SUCH_HOST, 9201);
    assert!((9200..=9299).contains(&forms::ERR_NO_SUCH_HOST));
}
