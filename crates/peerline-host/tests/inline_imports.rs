//! `imports { … => inline }`, over a real connection.
//!
//! # What this is for
//!
//! The macro crate's token tests can read what an inlined copy *says* — that
//! the struct is there, that it carries the vocabulary's prose, that the serde
//! attributes were emitted. What they cannot do is compare it to anything. The
//! claim the mode rests on is not "the copy has attributes" but "**the copy is
//! wire-identical to the expansion it replaces**", and a claim about two
//! expansions is only checkable with both of them in hand.
//!
//! So this file has both, and puts them on opposite ends of one connection:
//!
//! - `vocabulary` expands `demo.shared/1` on its own, the way the crate that
//!   exists to be an import target does.
//! - `by_path` expands `demo.relay/1` with `imports { "demo.shared/1" =>
//!   super::vocabulary }` — the mode that has always existed.
//! - `inlined` expands the *same* `demo.relay/1` with `imports {
//!   "demo.shared/1" => inline }`, generating its own copies.
//!
//! A producer built from one and a consumer built from the other exchange
//! values, in both directions, for every op. Nothing converts anything: the
//! bytes leaving one expansion are decoded by the other, which is exactly what
//! happens when two crates in a system pick different modes.
//!
//! # Why the fixture looks like that
//!
//! `demo.shared/1`'s `Envelope` carries a key in every state the format
//! distinguishes — absent-able, nullable, defaulted — plus a safe integer, a
//! closed set whose wire spelling is not its Rust name, and an internally
//! tagged union. Those are the six places a generated serde form could differ
//! from the manifest, and a difference in any of them is a failed decode here.
//!
//! `Body` is reached through `Envelope` and is **not** listed in the relay
//! contract's import, which is the other thing under test: a path mapping
//! never needed it named, because the other module had already compiled it,
//! and a copy does. If the closure the macro walks were only the listed names,
//! this file would not compile.
//!
//! # The cost the mode encodes, demonstrated rather than described
//!
//! `by_path::RelayArgs` holds a `vocabulary::Envelope`; `inlined::RelayArgs`
//! holds an `inlined::Envelope`. Those are two Rust types, in one crate, with
//! identical fields and no relationship — which is the whole trade, and
//! [`across`] below is what paying it looks like.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use futures::StreamExt;
use peerline::runtime::{Peer, loopback};
use peerline::wire::RpcError;

mod vocabulary {
    //! `demo.shared/1` on its own, as the crate whose only job is to be an
    //! import target expands it.
    peerline_contract::contract!("demo.shared/1" in "tests/contracts-inline");
}

mod by_path {
    //! The relay contract, with the vocabulary's Rust taken from the module
    //! above. One `Envelope` for both expansions.
    peerline_contract::contract!(
        "demo.relay/1" in "tests/contracts-inline",
        imports { "demo.shared/1" => super::vocabulary },
        runtime,
    );
}

mod inlined {
    //! The same relay contract, generating its own copies of the vocabulary's
    //! shapes. Nothing in here names `super::vocabulary`, and that is the
    //! point: this module would compile in a crate that had never heard of it.
    peerline_contract::contract!(
        "demo.relay/1" in "tests/contracts-inline",
        imports { "demo.shared/1" => inline },
        runtime,
    );
}

// ---------------------------------------------------------------------------
// The two producers, one per mode
// ---------------------------------------------------------------------------

/// A producer written against the path-mapped expansion.
#[derive(Default)]
struct PathSide {
    relayed: AtomicU32,
}

impl by_path::DemoRelayService for PathSide {
    async fn relay(&self, args: by_path::RelayArgs) -> Result<by_path::RelayResult, RpcError> {
        let seen = self.relayed.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(by_path::RelayResult::new(args.envelope, seen))
    }

    async fn echo(
        &self,
        args: by_path::EchoArgs,
        items: by_path::EchoItems,
    ) -> Result<(), RpcError> {
        for _ in 0..args.count {
            if items.send(&args.envelope).is_err() {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// The same producer written against the inlined expansion. Same code, a
/// different `Envelope`.
#[derive(Default)]
struct InlineSide {
    relayed: AtomicU32,
}

impl inlined::DemoRelayService for InlineSide {
    async fn relay(&self, args: inlined::RelayArgs) -> Result<inlined::RelayResult, RpcError> {
        let seen = self.relayed.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(inlined::RelayResult::new(args.envelope, seen))
    }

    async fn echo(
        &self,
        args: inlined::EchoArgs,
        items: inlined::EchoItems,
    ) -> Result<(), RpcError> {
        for _ in 0..args.count {
            if items.send(&args.envelope).is_err() {
                return Ok(());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The values, built once per mode from the same manifest
// ---------------------------------------------------------------------------

/// An envelope with every distinguishable key in a state worth checking: a
/// set whose wire spelling differs from its name, a union arm carrying a
/// field, an absent-able key that is present, a nullable key that is null,
/// and a defaulted key left at its manifest default.
fn path_envelope() -> vocabulary::Envelope {
    let mut envelope = vocabulary::Envelope::new(
        "e-1",
        1_700_000_000_000,
        vocabulary::Kind::FullMessage,
        vocabulary::Body::text("the body"),
    );
    envelope.note = Some("something the sender added".into());
    envelope.cause = None;
    envelope
}

/// The same value, built through the inlined copies.
fn inline_envelope() -> inlined::Envelope {
    let mut envelope = inlined::Envelope::new(
        "e-1",
        1_700_000_000_000,
        inlined::Kind::FullMessage,
        inlined::Body::text("the body"),
    );
    envelope.note = Some("something the sender added".into());
    envelope.cause = None;
    envelope
}

/// What crossing between two expansions of one vocabulary costs, written out
/// once so the tests below can be about the wire rather than about this.
///
/// The two types have identical fields and no relationship, so the conversion
/// is either field by field or through the wire form — and the wire form is
/// the one guaranteed to be right, because it is what both were generated to
/// produce. A crate that inlines a vocabulary and also holds the vocabulary's
/// own expansion writes something like this at the seam; a crate that inlines
/// and stops there never needs it, which is the case the mode is for.
fn across<A, B>(value: &A) -> B
where
    A: serde::Serialize,
    B: serde::de::DeserializeOwned,
{
    serde_json::from_value(serde_json::to_value(value).expect("a generated shape serialises"))
        .expect("the two expansions agree on the wire form")
}

// ---------------------------------------------------------------------------
// The wire form itself
// ---------------------------------------------------------------------------

/// **The assertion the feature rests on.** Both expansions were generated from
/// one manifest by one generator, so the JSON they produce for the same value
/// is not merely compatible — it is the same bytes, key for key.
///
/// Asserted against a literal as well as against each other, because "the two
/// agree" is also true if both are wrong in the same way. The literal is what
/// the manifest says: `full-message` and not `FullMessage`, `form` as the
/// discriminator beside the arm's own field, `note` present, `cause` written
/// as null, `hops` defaulted to three on the way *out* as well as in.
#[test]
fn the_two_expansions_produce_the_same_json_for_the_same_value() {
    let from_path = serde_json::to_value(path_envelope()).expect("serialises");
    let from_inline = serde_json::to_value(inline_envelope()).expect("serialises");
    assert_eq!(from_path, from_inline);

    assert_eq!(
        from_path,
        serde_json::json!({
            "id": "e-1",
            "at_ms": 1_700_000_000_000_i64,
            "kind": "full-message",
            "body": { "form": "text", "value": "the body" },
            "note": "something the sender added",
            "cause": null,
            "hops": 3
        }),
    );
}

/// An absent-able key is absent on the way out, and that is not a detail a
/// copy is allowed to lose: another language's decoder is entitled to tell
/// `note` missing from `note: null`, and the manifest distinguishes them.
#[test]
fn an_absent_key_stays_absent_through_both_expansions() {
    let mut path = path_envelope();
    path.note = None;
    let mut inline = inline_envelope();
    inline.note = None;

    let from_path = serde_json::to_value(path).expect("serialises");
    assert_eq!(from_path, serde_json::to_value(inline).expect("serialises"));

    let object = from_path.as_object().expect("an object");
    assert!(!object.contains_key("note"), "{object:?}");
    assert!(
        object.contains_key("cause"),
        "a nullable key is written even when null: {object:?}",
    );
}

/// The manifest's default is what an absent key means, in both expansions and
/// on the way in as well as out. `hops` defaults to three, which is not what
/// `u32::default()` would give — so a copy that derived `Default` instead of
/// writing it out would fail here.
#[test]
fn a_defaulted_key_means_the_same_thing_to_both() {
    let wire = serde_json::json!({
        "id": "e-2",
        "at_ms": 1_i64,
        "kind": "partial",
        "body": { "form": "empty" },
        "cause": null
    });
    let path: vocabulary::Envelope = serde_json::from_value(wire.clone()).expect("decodes");
    let inline: inlined::Envelope = serde_json::from_value(wire).expect("decodes");
    assert_eq!(path.hops, 3);
    assert_eq!(inline.hops, 3);
    assert_eq!(path.note, None);
    assert_eq!(inline.note, None);
    assert_eq!(inlined::Envelope::default_hops(), 3);
}

/// A generated set is closed in both expansions alike: an unrecognised tag is
/// a decoding failure at the boundary, not a value either side can hold. A
/// copy that had quietly gained a catch-all would make the inline mode more
/// permissive than the mode it replaces, which is a difference on the wire.
#[test]
fn an_unknown_variant_is_refused_by_both() {
    let wire = serde_json::json!({
        "id": "e-3",
        "at_ms": 1_i64,
        "kind": "invented-later",
        "body": { "form": "empty" },
        "cause": null
    });
    assert!(serde_json::from_value::<vocabulary::Envelope>(wire.clone()).is_err());
    assert!(serde_json::from_value::<inlined::Envelope>(wire).is_err());
}

// ---------------------------------------------------------------------------
// Over a connection, each mode answering the other
// ---------------------------------------------------------------------------

/// A producer on the path-mapped expansion, a client on the inlined one.
fn path_producer_inline_consumer() -> (inlined::DemoRelayClient, Arc<PathSide>, Peer) {
    let (consumer, producer, driver) = loopback();
    tokio::spawn(driver);
    let side = Arc::new(PathSide::default());
    by_path::register_demo_relay_handlers(&producer, Arc::clone(&side));
    (inlined::DemoRelayClient::new(consumer), side, producer)
}

/// And the mirror image, because wire compatibility is not a property of one
/// direction: a copy could encode correctly and decode leniently, or the
/// reverse, and only running it both ways finds that.
fn inline_producer_path_consumer() -> (by_path::DemoRelayClient, Arc<InlineSide>, Peer) {
    let (consumer, producer, driver) = loopback();
    tokio::spawn(driver);
    let side = Arc::new(InlineSide::default());
    inlined::register_demo_relay_handlers(&producer, Arc::clone(&side));
    (by_path::DemoRelayClient::new(consumer), side, producer)
}

/// **The test the feature exists to make passable.** Two expansions of one
/// contract, differing only in where the imported vocabulary's Rust came
/// from, on opposite ends of a connection — and the value survives the round
/// trip unchanged, in an op that names the imported shape in both its argument
/// and its result.
///
/// Nothing between the two converts anything. The inlined client encodes its
/// own `Envelope`; the path-mapped producer decodes `vocabulary::Envelope`
/// from those bytes, holds it, and sends it back.
#[tokio::test]
async fn an_inlined_consumer_and_a_path_mapped_producer_exchange_values() {
    let (client, side, _producer) = path_producer_inline_consumer();

    let sent = inline_envelope();
    let answer = client
        .relay(&inlined::RelayArgs::new(sent.clone()))
        .await
        .expect("the producer answered");

    assert_eq!(answer.envelope, sent, "the value came back as it went");
    assert_eq!(answer.seen, 1);
    assert_eq!(side.relayed.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn a_path_mapped_consumer_and_an_inlined_producer_exchange_values() {
    let (client, side, _producer) = inline_producer_path_consumer();

    let sent = path_envelope();
    let answer = client
        .relay(&by_path::RelayArgs::new(sent.clone()))
        .await
        .expect("the producer answered");

    assert_eq!(answer.envelope, sent);
    assert_eq!(answer.seen, 1);
    assert_eq!(side.relayed.load(Ordering::Relaxed), 1);
}

/// The third position an imported shape appears in, and the one with a
/// separate encoder: a stream item is put on the wire by the generated sink,
/// not by the unary result path. A copy that was right for a result and wrong
/// for an item would pass every test above.
#[tokio::test]
async fn an_imported_shape_crosses_the_two_modes_as_a_stream_item_too() {
    let (client, _side, _producer) = path_producer_inline_consumer();

    let mut items = client
        .echo(&inlined::EchoArgs::new(inline_envelope(), 3))
        .expect("the stream opened");

    let mut seen = Vec::new();
    while let Some(item) = items.next().await {
        seen.push(item.expect("an item, not a fault").data);
    }
    assert_eq!(seen, vec![inline_envelope(); 3]);
}

/// What the trade actually looks like at a seam, run rather than described.
///
/// This crate holds both expansions, so it has two `Envelope`s — which is the
/// case the crate docs warn can happen *within* one crate, not only between
/// two. Neither is convertible to the other by assignment, and the conversion
/// that does work is the wire form, because that is the one thing the two were
/// generated to agree on.
#[test]
fn a_value_crossing_between_two_expansions_converts_through_the_wire_form() {
    let from_vocabulary = path_envelope();
    let as_inlined: inlined::Envelope = across(&from_vocabulary);
    assert_eq!(as_inlined, inline_envelope());

    let back: vocabulary::Envelope = across(&as_inlined);
    assert_eq!(back, from_vocabulary);
}

/// The shape nobody listed. `Body` is reached through `Envelope` and is
/// absent from the relay contract's `imports`, so the inlined expansion had to
/// work out that it needed it — and this names the copy directly, which only
/// compiles if it did.
#[test]
fn a_shape_reached_through_a_listed_one_is_inlined_as_well() {
    let body = inlined::Body::text("held");
    assert_eq!(
        serde_json::to_value(&body).expect("serialises"),
        serde_json::to_value(vocabulary::Body::text("held")).expect("serialises"),
    );
    assert!(matches!(inlined::Body::empty(), inlined::Body::Empty));
}
