//! The `coord.*` wire form, asserted literally.
//!
//! Same discipline as `wire.rs`: a coordinator or a service written in another
//! language has to produce and read exactly these objects, so the tests pin the
//! JSON — field names, op names and error codes included — rather than only
//! that a value survives a round trip through itself.

use peerline_contract::coord::{
    AwaitReadyArgs, BindingGraph, ERR_INSTANCE_EXISTS, ERR_SINGLETON_OCCUPIED,
    ERR_UNKNOWN_INSTANCE, ERR_UNRESOLVED, Edge, Heartbeat, InstanceInfo, Listing, OP_AWAIT_READY,
    OP_BINDINGS, OP_HEARTBEAT, OP_LIST, OP_READY, OP_REGISTER, OP_RESOLVE, ReadyArgs, ReadyResult,
    RegisterAck, RegisterArgs, ResolveArgs, ResolveResult, ServiceEntry, SingletonOccupied,
    Unresolved,
};
use peerline_contract::{Binding, Consumed, ContractId, Endpoints, HostId, Produced};
use serde_json::{Value, json};

fn round_trip<T>(value: &T, expect: Value)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(value).expect("serialize");
    assert_eq!(encoded, expect, "wire form changed");
    let decoded: T = serde_json::from_value(expect).expect("deserialize");
    assert_eq!(&decoded, value, "round trip changed the value");
}

fn endpoints() -> Endpoints {
    let mut e = Endpoints::new(HostId::new("host-a"));
    e.uds.push("/tmp/storage.sock".into());
    e
}

#[test]
fn op_names_are_application_level_and_stable() {
    assert_eq!(OP_REGISTER, "coord.register");
    assert_eq!(OP_READY, "coord.ready");
    assert_eq!(OP_HEARTBEAT, "coord.heartbeat");
    assert_eq!(OP_RESOLVE, "coord.resolve");
    assert_eq!(OP_AWAIT_READY, "coord.await_ready");
    assert_eq!(OP_LIST, "coord.list");
    assert_eq!(OP_BINDINGS, "coord.bindings");

    // `$peerline/` is reserved for protocol ops; none of these may claim it.
    for op in [
        OP_REGISTER,
        OP_READY,
        OP_HEARTBEAT,
        OP_RESOLVE,
        OP_AWAIT_READY,
        OP_LIST,
    ] {
        assert!(
            !op.starts_with("$peerline/"),
            "{op} is in the reserved space"
        );
    }
}

#[test]
fn error_codes_are_outside_the_reserved_protocol_range() {
    for code in [
        ERR_INSTANCE_EXISTS,
        ERR_UNKNOWN_INSTANCE,
        ERR_UNRESOLVED,
        ERR_SINGLETON_OCCUPIED,
    ] {
        assert!(
            !(-32603..=-32600).contains(&code),
            "{code} collides with peerline's reserved range"
        );
    }
    assert_ne!(ERR_INSTANCE_EXISTS, ERR_UNKNOWN_INSTANCE);
    assert_ne!(ERR_UNKNOWN_INSTANCE, ERR_UNRESOLVED);
    // Two producers of a singleton is not two claimants of one id, and a
    // caller that matched on one code for both would print the wrong fix.
    assert_ne!(ERR_SINGLETON_OCCUPIED, ERR_INSTANCE_EXISTS);
    assert_ne!(ERR_SINGLETON_OCCUPIED, ERR_UNRESOLVED);
}

#[test]
fn singleton_occupied_names_both_ends() {
    round_trip(
        &SingletonOccupied {
            contract: ContractId::new("storage", 1),
            instance: "storage-b".into(),
            holder: "storage-a".into(),
            disagreed: false,
        },
        json!({
            "contract": { "name": "storage", "version": 1 },
            "instance": "storage-b",
            "holder": "storage-a",
            "disagreed": false,
        }),
    );
}

#[test]
fn service_entry_carries_endpoints_per_mounted_service() {
    let mut entry = ServiceEntry::new("storage", endpoints());
    entry.produces.push(Produced::new("storage", 1));
    entry.consumes.push(Consumed::optional("index", 1));
    round_trip(
        &entry,
        json!({
            "service": "storage",
            "endpoints": { "host": "host-a", "ws": [], "uds": ["/tmp/storage.sock"], "iroh": [] },
            "produces": [{
                "contract": { "name": "storage", "version": 1 },
                "ops": [],
                "cardinality": "any",
            }],
            "consumes": [{ "contract": { "name": "index", "version": 1 }, "optional": true }],
        }),
    );
}

#[test]
fn a_service_declaring_nothing_may_omit_both_lists() {
    let decoded: ServiceEntry = serde_json::from_value(json!({
        "service": "storage",
        "endpoints": { "host": "host-a" },
    }))
    .unwrap();
    assert!(decoded.produces.is_empty() && decoded.consumes.is_empty());
}

#[test]
fn register_carries_a_service_supplied_instance_id() {
    let mut entry = ServiceEntry::new("storage", endpoints());
    entry.produces.push(Produced::new("storage", 1));
    round_trip(
        &RegisterArgs {
            instance: "storage@/tmp/storage.sock".into(),
            pid: 4242,
            services: vec![entry],
        },
        json!({
            "instance": "storage@/tmp/storage.sock",
            "pid": 4242,
            "services": [{
                "service": "storage",
                "endpoints": { "host": "host-a", "ws": [], "uds": ["/tmp/storage.sock"], "iroh": [] },
                "produces": [{
                    "contract": { "name": "storage", "version": 1 },
                    "ops": [],
                    "cardinality": "any",
                }],
                "consumes": [],
            }],
        }),
    );
    // The id is echoed, not assigned: a caller confirms the coordinator holds
    // the identity it thinks it does.
    round_trip(
        &RegisterAck {
            id: "storage@/tmp/storage.sock".into(),
        },
        json!({ "id": "storage@/tmp/storage.sock" }),
    );
}

#[test]
fn ready_answers_a_tagged_status_not_a_bare_boolean() {
    // A caller in any language switches on one field; `unknown_instance` means
    // "re-register, then say ready again".
    round_trip(&ReadyResult::Ok, json!({ "status": "ok" }));
    round_trip(
        &ReadyResult::UnknownInstance,
        json!({ "status": "unknown_instance" }),
    );
    round_trip(
        &ReadyArgs {
            instance: "storage".into(),
        },
        json!({ "instance": "storage" }),
    );
}

#[test]
fn heartbeat_carries_only_the_instance() {
    round_trip(
        &Heartbeat {
            instance: "storage".into(),
        },
        json!({ "instance": "storage" }),
    );
}

#[test]
fn resolve_returns_ready_producers_best_first() {
    round_trip(
        &ResolveArgs::new(ContractId::new("storage", 1)),
        // An anonymous ask — a CLI, with no identity to record a selection
        // against — omits `instance` entirely rather than sending `null`.
        json!({ "contract": { "name": "storage", "version": 1 } }),
    );
    round_trip(
        &ResolveArgs::by("fs-1", ContractId::new("storage", 1)),
        json!({ "contract": { "name": "storage", "version": 1 }, "instance": "fs-1" }),
    );
    round_trip(
        &ResolveResult {
            bindings: vec![Binding {
                contract: ContractId::new("storage", 1),
                instance: "storage-a".into(),
                endpoints: endpoints(),
            }],
        },
        json!({ "bindings": [{
            "contract": { "name": "storage", "version": 1 },
            "instance": "storage-a",
            "endpoints": { "host": "host-a", "ws": [], "uds": ["/tmp/storage.sock"], "iroh": [] },
        }] }),
    );
    // No ready producer is an empty answer, not an error.
    let decoded: ResolveResult = serde_json::from_value(json!({})).unwrap();
    assert!(decoded.bindings.is_empty());
}

#[test]
fn await_ready_does_not_re_declare_what_register_established() {
    round_trip(
        &AwaitReadyArgs {
            instance: "fs".into(),
            timeout_ms: 30_000,
        },
        json!({ "instance": "fs", "timeout_ms": 30000 }),
    );

    // Two sources of truth for one fact on adjacent calls is the bug this
    // shape exists to avoid: the contracts waited for come from `register`.
    let encoded = serde_json::to_value(AwaitReadyArgs {
        instance: "fs".into(),
        timeout_ms: 1,
    })
    .unwrap();
    assert!(encoded.get("consumes").is_none());
    assert!(encoded.get("needs").is_none());
}

#[test]
fn a_timeout_names_the_unresolved_contracts_and_any_suspected_cycle() {
    round_trip(
        &Unresolved {
            instance: "storage-cache".into(),
            waited_ms: 30_000,
            unresolved: vec![ContractId::new("index", 1)],
            cycle: vec![
                ContractId::new("index", 1),
                ContractId::new("storage", 1),
                ContractId::new("index", 1),
            ],
        },
        json!({
            "instance": "storage-cache",
            "waited_ms": 30000,
            "unresolved": [{ "name": "index", "version": 1 }],
            "cycle": [
                { "name": "index", "version": 1 },
                { "name": "storage", "version": 1 },
                { "name": "index", "version": 1 },
            ],
        }),
    );

    // Advisory: a timeout with no cycle in sight simply omits it.
    let decoded: Unresolved = serde_json::from_value(json!({
        "instance": "fs",
        "waited_ms": 150,
        "unresolved": [{ "name": "storage", "version": 1 }],
    }))
    .unwrap();
    assert!(decoded.cycle.is_empty());
}

#[test]
fn list_reports_readiness_and_who_is_waiting() {
    round_trip(
        &Listing {
            ttl_ms: 15_000,
            address: "/tmp/peerline-coord.sock".into(),
            instances: vec![InstanceInfo {
                instance: "fs".into(),
                pid: 4242,
                ready: false,
                last_seen_ms: 12,
                services: vec![
                    ServiceEntry::new("fs", endpoints()),
                    // A service of this instance that bound nothing at all
                    // (§8) — registered, and dialable by nobody.
                    ServiceEntry::new("fs-iroh", Endpoints::new(HostId::new("host-a"))),
                ],
                waiting_on: vec![ContractId::new("storage", 1)],
                unreachable: vec!["fs-iroh".into()],
            }],
        },
        json!({
            "ttl_ms": 15_000,
            "address": "/tmp/peerline-coord.sock",
            "instances": [{
            "instance": "fs",
            "pid": 4242,
            "ready": false,
            "last_seen_ms": 12,
            "services": [{
                "service": "fs",
                "endpoints": { "host": "host-a", "ws": [], "uds": ["/tmp/storage.sock"], "iroh": [] },
                "produces": [],
                "consumes": [],
            }, {
                "service": "fs-iroh",
                "endpoints": { "host": "host-a", "ws": [], "uds": [], "iroh": [] },
                "produces": [],
                "consumes": [],
            }],
            "waiting_on": [{ "name": "storage", "version": 1 }],
            "unreachable": ["fs-iroh"],
        }] }),
    );
    let empty: Listing = serde_json::from_value(json!({})).unwrap();
    assert!(empty.instances.is_empty());
    assert_eq!(
        empty.ttl_ms, 0,
        "a coordinator too old to report its TTL reads as 0, not as a default \
         a caller would then compare `last_seen_ms` against"
    );
    assert!(empty.address.is_empty());

    // `unreachable` is additive: a coordinator too old to report it reads as
    // "nothing known to be unreachable", not as an absent field.
    let older: InstanceInfo = serde_json::from_value(json!({
        "instance": "fs",
        "pid": 0,
        "ready": true,
        "last_seen_ms": 0,
        "services": [],
    }))
    .unwrap();
    assert!(older.unreachable.is_empty());
}

/// The op that says what was actually *chosen*, as against what `coord.list`
/// says each instance declares.
#[test]
fn bindings_report_the_selection_that_was_made() {
    round_trip(
        &BindingGraph {
            edges: vec![Edge {
                consumer: "fs-1".into(),
                contract: ContractId::new("storage", 1),
                producer: "storage-a".into(),
                age_ms: 1_500,
            }],
        },
        json!({ "edges": [{
            "consumer": "fs-1",
            "contract": { "name": "storage", "version": 1 },
            "producer": "storage-a",
            "age_ms": 1_500,
        }] }),
    );
    let empty: BindingGraph = serde_json::from_value(json!({})).unwrap();
    assert!(
        empty.edges.is_empty(),
        "a coordinator that has selected nothing yet answers an empty graph, \
         which is not the same as being unreachable"
    );
}
