//! Compatibility with the bindings a downstream already has.
//!
//! coagent generates TypeScript for three shapes of its `coagent.rpc/1`
//! vocabulary the long way round — the manifest expands into Rust, a
//! `typescript { BackendKind, CoagentEventKind, SessionInfo }` clause puts
//! `#[derive(ts_rs::TS)]` on those three, and a `bindings-export` binary runs
//! `TS::export_all`. This suite emits the same three shapes *directly* from
//! the same manifest and compares.
//!
//! # Why the fixtures are committed copies and not a path into that repository
//!
//! Two reasons, and the second is the one that decides it.
//!
//! A test that reads `../../../coagent` passes or fails depending on what is
//! checked out beside this repository, which makes it a test of somebody's
//! working copy. `peerline-host` is published, and a crate whose test suite
//! reaches outside its own package cannot be verified from a `cargo package`
//! tarball at all.
//!
//! And the generated tree is **gitignored** in that repository. It is build
//! output, recreated by `just wasm`; there is no committed baseline over
//! there to point at even if pointing were acceptable. So `tests/compat/tsrs/`
//! holds a copy taken from a real `bindings-export` run — not a
//! reconstruction, not a guess at what `ts_rs` would emit — and
//! `tests/compat/contracts/` holds the manifest byte-for-byte as that
//! repository has it. Both are inputs to a test, which is what a fixture is.
//!
//! When coagent's manifest changes, these stop describing it. That is a
//! property of every snapshot, and the mitigation is that the manifest copy is
//! diffable in one command.
//!
//! # The result, in one paragraph
//!
//! **Not byte-identical, and identical in every type it declares.** After
//! erasing comments and normalising whitespace and the member separator, the
//! generated declarations match `ts_rs`'s exactly — same names, same members,
//! same order, same types, same imports. Every difference is layout or is
//! prose that `ts_rs` had no way to carry. [`the_divergences`] enumerates them
//! individually and asserts each one, so the list cannot quietly grow.

use std::collections::BTreeMap;
use std::path::PathBuf;

use peerline_contract_spec::Directory;
use peerline_contract_ts::{Options, emit_directory};

/// The three shapes coagent's `typescript { … }` clause names.
const EXPORTED: [&str; 3] = ["BackendKind", "CoagentEventKind", "SessionInfo"];

fn base() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/compat")
}

fn ours() -> BTreeMap<PathBuf, String> {
    let dir = Directory::load(&base().join("contracts"))
        .expect("coagent's `coagent.rpc/1` manifest must parse and validate here too");
    // The default options are the compatible ones by construction, except for
    // `DefaultFields` — and `coagent.rpc/1` declares no defaulted field, so
    // that choice cannot reach this comparison. `default_fields_never_arises`
    // below is that statement as a test rather than as a comment.
    emit_directory(&dir, &Options::default())
        .expect("one manifest cannot declare a type name twice; the validator said so")
}

fn ours_file(name: &str) -> String {
    ours()[&PathBuf::from(format!("{name}.ts"))].clone()
}

fn theirs_file(name: &str) -> String {
    let path = base().join("tsrs").join(format!("{name}.ts"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// A declaration reduced to what it *says*, with everything about how it is
/// laid out removed.
///
/// This is the whole comparison, so it is worth being precise about what it
/// erases and what it does not. It erases: comments, all runs of whitespace,
/// the choice of `,` or `;` between object members, a trailing separator
/// before `}`, and a leading `|` on a union. It keeps: every identifier,
/// every type expression, every string literal, every `?`, the order of
/// members, and the import statements. Two declarations that normalise alike
/// describe the same TypeScript type, and a consumer cannot tell them apart.
fn normalise(text: &str) -> String {
    let no_comments = strip_comments(text);
    // Collapse whitespace, then re-normalise the separators. Doing it in this
    // order means `a: string,   }` and `a: string;\n}` reach the same place.
    let mut out = String::with_capacity(no_comments.len());
    let mut space = false;
    for ch in no_comments.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(ch);
    }
    out.replace(", }", " }")
        .replace("; }", " }")
        .replace(',', ";")
        .replace("= | ", "= ")
        .replace("; |", " |")
        .trim()
        .to_string()
}

/// Remove `//` line comments and `/* … */` blocks.
///
/// Nothing in either input has a comment marker inside a string literal —
/// every string here is a wire value from the manifest, which the format
/// requires to be a non-blank string and which in practice are lowercase
/// hyphenated words — so the simple scanner is sufficient and its limits are
/// worth stating rather than hiding.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '/' && i + 1 < bytes.len() && bytes[i + 1] == '/' {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == '*' && bytes[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            out.push(' ');
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// The headline: every one of the three exported shapes declares exactly the
/// type `ts_rs` declares for it.
#[test]
fn the_three_exported_shapes_declare_the_same_types() {
    for name in EXPORTED {
        let ours = normalise(&ours_file(name));
        let theirs = normalise(&theirs_file(name));
        assert_eq!(
            ours, theirs,
            "`{name}` would change meaning if a consumer swapped generators.\n\
             \n  ts-rs:     {theirs}\n  this crate: {ours}\n"
        );
    }
}

/// The three facts that motivated the `typescript` clause reading the
/// manifest, checked against the file it produced.
///
/// These are the cases where `ts_rs` looking at the generated *Rust* gets the
/// wrong answer and the macros crate patches it back from the manifest. This
/// generator never has the wrong answer to patch, so the assertion is that
/// both arrive at the same place by different routes.
#[test]
fn the_portability_facts_survive_both_routes() {
    let ours = ours_file("SessionInfo");
    let theirs = theirs_file("SessionInfo");

    // Declared `i53`. Through Rust it is an `i64`, which ts_rs calls
    // `bigint`, which the clause overrides back to `number`. Here it is
    // `number` because the manifest said `i53`.
    assert!(theirs.contains("created_at_ms: number"), "{theirs}");
    assert!(ours.contains("created_at_ms: number;"), "{ours}");

    // Declared `json`. Through Rust it is a `serde_json::Value`, which ts_rs
    // turns into an imported `JsonValue`, overridden back to `unknown`.
    assert!(theirs.contains("config: unknown"), "{theirs}");
    assert!(ours.contains("config: unknown;"), "{ours}");
    assert!(
        !ours.contains("JsonValue"),
        "no import of a JSON value type should appear:\n{ours}"
    );

    // Wire spellings, which the clause has to state explicitly because ts_rs
    // only reads serde attributes behind a Cargo feature.
    for wire in ["\"claude-code\"", "\"codex\""] {
        assert!(theirs_file("BackendKind").contains(wire));
        assert!(ours_file("BackendKind").contains(wire));
    }
}

/// All fifteen event kinds, in order, with their wire spellings.
///
/// Spelled out rather than left to the normalised comparison because this is
/// the enum a browser switches on: a value quietly dropped or respelled is a
/// consumer that stops rendering something, and a diff in a normalised blob
/// is a worse way to learn that than a list.
#[test]
fn every_event_kind_travels_under_its_wire_spelling() {
    const KINDS: [&str; 15] = [
        "session-started",
        "session-stopped",
        "turn-started",
        "turn-stopped",
        "message-started",
        "partial-message",
        "message-stopped",
        "full-message",
        "usage-updated",
        "error",
        "compact-started",
        "compact-stopped",
        "compacted-context",
        "backend-notification",
        "raw",
    ];
    let ours = ours_file("CoagentEventKind");
    let theirs = theirs_file("CoagentEventKind");
    for kind in KINDS {
        let literal = format!("\"{kind}\"");
        assert!(ours.contains(&literal), "this crate dropped `{kind}`");
        assert!(theirs.contains(&literal), "the fixture lost `{kind}`");
    }
    // And nothing else: a sixteenth value appearing would be a set that is no
    // longer closed, which is the property the manifest's own prose leans on.
    assert_eq!(
        ours.matches('"').count() - 2 * ours.matches("from \"").count(),
        KINDS.len() * 2,
        "exactly fifteen string literals, and no more"
    );
}

/// Imports match, including the direction of the dependency.
#[test]
fn the_import_graph_is_the_same() {
    assert!(
        ours_file("SessionInfo").contains("import type { BackendKind } from \"./BackendKind\";"),
        "the same specifier ts_rs writes, so a file swapped in place resolves"
    );
    assert!(
        theirs_file("SessionInfo").contains("import type { BackendKind } from \"./BackendKind\";")
    );
    for name in ["BackendKind", "CoagentEventKind"] {
        assert!(
            !ours_file(name).contains("import"),
            "`{name}` is a string union and depends on nothing"
        );
    }
}

/// The file layout is the one the existing tooling expects.
#[test]
fn the_layout_is_one_file_per_type_named_after_it() {
    let files = ours();
    for name in EXPORTED {
        assert!(
            files.contains_key(&PathBuf::from(format!("{name}.ts"))),
            "`{name}.ts` is the file ts_rs writes and the one a consumer imports"
        );
    }
    // The other three shapes of the vocabulary. coagent's clause does not
    // name them — it names only what a browser imports — but this generator
    // reads the manifest rather than a clause, so it emits the whole file.
    // That is a *superset*, which is safe: nothing that existed changed.
    for name in ["EventDirection", "EventMeta", "StartParams"] {
        assert!(
            files.contains_key(&PathBuf::from(format!("{name}.ts"))),
            "`{name}` is declared in the manifest and should be emitted"
        );
    }
    assert_eq!(
        files.len(),
        6,
        "six shapes, six files, no barrel by default"
    );
}

/// `coagent.rpc/1` has no defaulted field, so the one mapping decision that
/// departs from the `ts_rs` route cannot affect this comparison.
///
/// Worth a test rather than a comment: if that manifest ever grows a default,
/// this fails and points at the `--defaults required` escape hatch before a
/// swap silently changes a key from required to optional.
#[test]
fn the_one_divergent_decision_never_arises_here() {
    let dir = Directory::load(&base().join("contracts")).expect("the manifest");
    for manifest in dir.manifests() {
        for def in &manifest.types {
            let peerline_contract_spec::TypeDef::Struct { name, fields, .. } = def else {
                continue;
            };
            for field in fields {
                assert!(
                    field.default.is_none(),
                    "`{name}.{}` now carries a default, so this crate would emit `key?:` \
                     where the ts-rs route emits `key:`. Either accept the change or pass \
                     `--defaults required` for this directory.",
                    field.name
                );
            }
        }
    }
}

/// Every way the output differs from `ts_rs`'s, enumerated and asserted.
///
/// The point of asserting a *divergence* is that the list cannot grow without
/// somebody writing it down. Each entry below is a deliberate choice with its
/// reasoning in the crate docs; anything not on this list that starts
/// differing will fail [`the_three_exported_shapes_declare_the_same_types`]
/// instead, which is where it belongs.
#[test]
fn the_divergences() {
    let ours = ours_file("SessionInfo");
    let theirs = theirs_file("SessionInfo");

    // 1. The banner names the manifest, not the tool that read it — because
    //    the question a reader with a wrong type has is "where do I change
    //    this", and the answer is a `.json` file.
    assert!(theirs.starts_with("// This file was generated by [ts-rs]"));
    assert!(ours.starts_with("// Generated by peerline-contract-ts from the `coagent.rpc/1`"));
    assert!(ours.contains("change `coagent.rpc.v1.json` and regenerate"));

    // 2. Members are separated by `;` and indented two spaces. ts-rs emits
    //    `,` and no indentation at all. `;` is what Prettier writes for a
    //    multi-line object type, so a repository that formats its generated
    //    tree leaves this alone instead of rewriting it into a permanent diff.
    assert!(ours.contains("  session_id: string;"));
    assert!(theirs.contains("session_id: string, "));

    // 3. ts-rs leaves a trailing space after every member and after the
    //    opening brace. Nothing here does; see the golden suite's
    //    `the_output_is_stable_under_the_formatters_it_will_meet`.
    assert!(theirs.contains("= { \n") || theirs.contains("string, \n"));
    assert!(ours.lines().all(|l| l.trim_end() == l));

    // 4. An enum's variant prose. The manifest requires a description on
    //    every value and the format's stated reason is that a generator emits
    //    it as comments in its own target; ts-rs has nowhere to put it,
    //    because a Rust unit variant's doc comment is not carried onto a
    //    member of a string union. This is the one divergence that is a
    //    capability rather than a preference.
    let ours_enum = ours_file("BackendKind");
    let theirs_enum = theirs_file("BackendKind");
    // `BackendKind.claude_code`'s own description in `coagent.rpc.v1.json`.
    let variant_prose = "one long-lived child process per session";
    assert!(
        ours_enum.contains(variant_prose),
        "the variant's own prose reaches TypeScript:\n{ours_enum}"
    );
    assert!(
        !theirs_enum.contains(variant_prose),
        "ts-rs carries the type's prose but not the variant's — if this ever \
         starts failing, the divergence has closed and this test should say so"
    );
    // The type-level prose does survive both routes, which is what makes the
    // gap specifically about variants rather than about enums.
    let type_prose = "Which agent backend a session is running against.";
    assert!(ours_enum.contains(type_prose) && theirs_enum.contains(type_prose));

    // 5. The type-level prose is identical, which is worth pinning: it is the
    //    manifest's, both ways, and a difference would mean one of the two
    //    routes is reformatting it.
    let quote = "Everything here is decided when the session is created and never changes";
    assert!(ours.contains(quote) && theirs.contains(quote));

    // 6. This generator emits every shape the manifest declares; the ts-rs
    //    route emits what the `typescript` clause names. A superset, asserted
    //    in `the_layout_is_one_file_per_type_named_after_it`.
}
