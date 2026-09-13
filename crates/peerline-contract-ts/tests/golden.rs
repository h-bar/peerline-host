//! Golden files: every feature of the manifest format, emitted and committed.
//!
//! `tests/contracts/` holds two manifests written as a checklist rather than
//! as a design — see their own prose — and `tests/golden/` holds exactly what
//! this crate makes of them, in both layouts. A change to the emitter that
//! nobody meant shows up here as a diff in a file a human can read, which is
//! the only form of review a code generator's output actually gets.
//!
//! # Re-blessing
//!
//! ```text
//! BLESS=1 cargo test -p peerline-contract-ts --test golden
//! ```
//!
//! Then **read the diff**. A golden file that is re-blessed without being read
//! is a golden file that has stopped testing anything, and this suite's whole
//! value is that the output of a generator is hard to be wrong about in the
//! abstract and easy to be wrong about on the page.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use peerline_contract_spec::Directory;
use peerline_contract_ts::{DefaultFields, Layout, Options, WideIntegers, emit_directory};

fn contracts() -> Directory {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contracts");
    Directory::load(&dir).expect("the fixture contracts directory must parse and validate")
}

fn golden_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn blessing() -> bool {
    std::env::var_os("BLESS").is_some()
}

/// Compare a generated tree against a committed one, or write it.
fn check_tree(subdir: &str, generated: &BTreeMap<PathBuf, String>) {
    let root = golden_root().join(subdir);
    if blessing() {
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("creating the golden directory");
        for (name, text) in generated {
            std::fs::write(root.join(name), text).expect("writing a golden file");
        }
        return;
    }

    let mut problems: Vec<String> = Vec::new();
    for (name, want) in generated {
        let path = root.join(name);
        match std::fs::read_to_string(&path) {
            Ok(have) if have == *want => {}
            Ok(have) => problems.push(format!(
                "{}/{}: differs at {}",
                subdir,
                name.display(),
                first_difference(&have, want)
            )),
            Err(_) => problems.push(format!("{subdir}/{}: not committed", name.display())),
        }
    }
    // A golden file left behind by a type that has been renamed away would
    // otherwise sit there passing forever, which is the failure a
    // contents-only comparison cannot see.
    for name in committed_ts(&root) {
        if !generated.contains_key(&name) {
            problems.push(format!(
                "{subdir}/{}: committed but no longer generated",
                name.display()
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the generated TypeScript has drifted from `tests/golden/`:\n  {}\n\n\
         If the change is intended, re-bless with `BLESS=1 cargo test -p \
         peerline-contract-ts --test golden` and read the diff.",
        problems.join("\n  ")
    );
}

fn committed_ts(root: &Path) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    listing
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ts"))
        .filter_map(|p| p.file_name().map(PathBuf::from))
        .collect()
}

fn first_difference(have: &str, want: &str) -> String {
    for (n, (a, b)) in have.lines().zip(want.lines()).enumerate() {
        if a != b {
            return format!("line {}\n      committed: {a}\n      generated: {b}", n + 1);
        }
    }
    format!(
        "the end: committed has {} lines, generated has {}",
        have.lines().count(),
        want.lines().count()
    )
}

/// The whole checklist, one file per named type — the default layout, and the
/// one the existing `ts_rs` bindings already use.
#[test]
fn one_file_per_type() {
    let opts = Options {
        barrel: true,
        ..Options::default()
    };
    check_tree("type", &emit_directory(&contracts(), &opts));
}

/// The same shapes, one file per manifest. Worth committing separately
/// because the import resolution is genuinely different: under this layout a
/// reference to a sibling shape needs no import at all, and only a reference
/// across manifests does.
#[test]
fn one_file_per_manifest() {
    let opts = Options {
        layout: Layout::Manifest,
        barrel: true,
        ..Options::default()
    };
    check_tree("manifest", &emit_directory(&contracts(), &opts));
}

// ---------------------------------------------------------------------------
// The feature checklist, asserted by name.
//
// The golden trees above already contain all of this. These tests exist so
// that a failure *names the feature* rather than pointing at a line number in
// a large file — and so that the list of things this generator claims to
// handle is readable in one place.
// ---------------------------------------------------------------------------

fn generated() -> BTreeMap<PathBuf, String> {
    emit_directory(&contracts(), &Options::default())
}

fn file(name: &str) -> String {
    let files = generated();
    files
        .get(&PathBuf::from(name))
        .unwrap_or_else(|| panic!("`{name}` was not generated; files: {:?}", files.keys()))
        .clone()
}

fn assert_has(haystack: &str, needle: &str, why: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find `{needle}` ({why}), in:\n{haystack}"
    );
}

/// Every scalar keyword, once each. If this list stops being exhaustive the
/// compiler-enforced match in `scalar.rs` is what says so; this is what says
/// the answers are the intended ones.
#[test]
fn every_scalar_maps_to_its_documented_typescript() {
    let ts = file("Scalars.ts");
    for (field, expected) in [
        ("text", "string"),
        ("flag", "boolean"),
        ("small_signed", "number"),
        ("small_unsigned", "number"),
        // The whole point of the safe-integer keywords: exact in a double,
        // so `number` and not `bigint`.
        ("safe_signed", "number"),
        ("safe_unsigned", "number"),
        // And the whole point of the wide ones: the `bigint` is the warning.
        ("wide_signed", "bigint"),
        ("wide_unsigned", "bigint"),
        ("fraction", "number"),
        ("where_it_runs", "string"),
        ("passthrough", "unknown"),
        ("lines", "Array<string>"),
        ("directions", "Array<Direction>"),
    ] {
        assert_has(
            &ts,
            &format!("{field}: {expected};"),
            "the scalar table in scalar.rs",
        );
    }
}

/// The three field states, plus required, are four different lines.
#[test]
fn the_three_field_states_are_three_different_types() {
    let ts = file("States.ts");
    assert_has(&ts, "required_text: string;", "required: no `?`, no `null`");
    assert_has(
        &ts,
        "optional_text?: string;",
        "optional: `?` and never `| null` — a writer omits the key",
    );
    assert_has(
        &ts,
        "nullable_text: string | null;",
        "nullable: `| null` and never `?` — the key is always written",
    );
    assert_has(
        &ts,
        "include_history?: boolean;",
        "default: `?`, because absence is legal and has a meaning",
    );
    assert_has(
        &ts,
        "Absent means `true`.",
        "a default's *value* is the one thing the type cannot carry, so it \
         goes in the doc comment",
    );
    assert_has(
        &ts,
        "Absent means `\"claude-code\"`.",
        "an enum default is rendered as its wire spelling",
    );
    assert_has(
        &ts,
        "Absent means `{}`.",
        "a struct default is rendered whole",
    );
}

/// `--defaults required` reproduces what the `ts_rs` route emits for a
/// defaulted field, for a downstream that wants the swap byte-identical
/// first.
#[test]
fn defaults_can_be_emitted_the_way_ts_rs_does() {
    let opts = Options {
        defaults: DefaultFields::Required,
        ..Options::default()
    };
    let files = emit_directory(&contracts(), &opts);
    let ts = &files[&PathBuf::from("States.ts")];
    assert_has(ts, "include_history: boolean;", "no `?` under this option");
    assert!(
        !ts.contains("include_history?:"),
        "the option must actually change the emitted key"
    );
}

/// The opt-out for the wide integers, and the fact that it does not touch the
/// safe ones — which is the entire distinction the keywords exist to make.
#[test]
fn wide_integers_can_be_narrowed_and_safe_ones_are_unaffected() {
    let opts = Options {
        wide_integers: WideIntegers::Number,
        ..Options::default()
    };
    let files = emit_directory(&contracts(), &opts);
    let ts = &files[&PathBuf::from("Scalars.ts")];
    assert_has(ts, "wide_signed: number;", "`i64` narrowed by request");
    assert_has(ts, "wide_unsigned: number;", "`u64` narrowed by request");
    assert!(
        !ts.contains("bigint"),
        "nothing should be left as a bigint under this option"
    );
    assert_has(ts, "safe_signed: number;", "`i53` was already `number`");
}

/// An enum is a union of **wire** spellings, and the variant names never
/// appear: TypeScript's member is the string itself.
#[test]
fn an_enum_is_a_string_literal_union_of_wire_spellings() {
    let ts = file("Backend.ts");
    assert_has(&ts, "| \"claude-code\"", "the respelled variant");
    assert_has(
        &ts,
        "| \"codex\"",
        "the variant whose name is its wire value",
    );
    assert!(
        !ts.contains("claude_code"),
        "the manifest's identifier spelling is not a TypeScript member; only \
         the wire value is:\n{ts}"
    );
    assert_has(
        &ts,
        "Two words on the wire, hyphenated.",
        "a variant's prose survives, which is the thing the ts-rs route drops",
    );
}

/// An internally tagged union is a discriminated union on the tag key, which
/// is what TypeScript narrows a `switch` on.
#[test]
fn an_internally_tagged_union_discriminates_on_the_tag() {
    let ts = file("Item.ts");
    assert_has(
        &ts,
        "({ item: \"event\" } & Event)",
        "a payload arm is an intersection: the payload's keys sit beside the tag",
    );
    assert_has(&ts, "item: \"gap\";", "a fields arm carries the tag first");
    assert_has(&ts, "lost: bigint;", "and then its own fields");
    assert_has(&ts, "note?: string;", "including their field states");
    assert_has(
        &ts,
        "item: \"finished\";",
        "an arm with neither payload nor fields is still an object with a tag, \
         and it travels under its wire spelling",
    );
}

/// An externally tagged union is one key, which is the arm name — including
/// the case internal tagging cannot express at all.
#[test]
fn an_externally_tagged_union_keys_on_the_arm_name() {
    let ts = file("Choice.ts");
    assert_has(
        &ts,
        "only: Array<Backend>;",
        "a list payload, which is the reason external tagging exists",
    );
    assert_has(
        &ts,
        "\"by-name\": {",
        "an arm whose wire spelling is not an identifier must be quoted",
    );
    assert_has(&ts, "prefix: string;", "and its own fields nest under it");
    assert_has(
        &ts,
        "| \"everything\";",
        "an arm with no payload and no fields is a bare string, because that \
         is what serde's external tagging writes for a unit variant",
    );
}

/// A flattened field's keys are its parent's, so the type is an intersection.
#[test]
fn flatten_is_an_intersection() {
    assert_has(
        &file("StartArgs.ts"),
        "} & Request;",
        "the object half, intersected with what it flattens",
    );
    assert_has(
        &file("Envelope.ts"),
        "export type Envelope = Request & Trace;",
        "a struct whose fields are *all* flattened has no object half at all — \
         an empty `{}` would widen the intersection to nothing useful",
    );
}

/// A struct with no fields is not a struct that accepts anything.
#[test]
fn an_empty_struct_is_not_an_open_one() {
    assert_has(
        &file("Ack.ts"),
        "export type Ack = Record<string, never>;",
        "`{}` in TypeScript means `anything but null and undefined`",
    );
}

/// Imports across manifests, resolved to the other file.
#[test]
fn a_shape_from_another_manifest_is_imported() {
    assert_has(
        &file("Event.ts"),
        "import type { Meta } from \"./Meta\";",
        "under the per-type layout every reference is its own file",
    );

    let opts = Options {
        layout: Layout::Manifest,
        ..Options::default()
    };
    let files = emit_directory(&contracts(), &opts);
    let ts = &files[&PathBuf::from("demo.every.v1.ts")];
    assert_has(
        ts,
        "import type { Direction, Meta } from \"./demo.vocab.v1\";",
        "under the per-manifest layout only cross-manifest names are imported, \
         and the specifier is the other manifest's own file name",
    );
    assert!(
        !ts.contains("from \"./Backend\""),
        "a sibling shape in the same file needs no import:\n{ts}"
    );
}

/// Prose reaches TypeScript, which is the reason every description in the
/// format is required.
#[test]
fn the_manifests_prose_becomes_doc_comments() {
    let ts = file("Scalars.ts");
    assert_has(
        &ts,
        " * Every scalar keyword the format has, once each, plus the two ways a list is written.",
        "a type's description",
    );
    assert_has(&ts, " * UTF-8 text.", "a field's description");
    assert_has(&ts, " *\n", "a blank entry is a paragraph break");
}

/// The barrel, when asked for.
#[test]
fn the_barrel_re_exports_every_shape() {
    let opts = Options {
        barrel: true,
        ..Options::default()
    };
    let files = emit_directory(&contracts(), &opts);
    let index = &files[&PathBuf::from("index.ts")];
    assert_has(
        index,
        "export type { Meta } from \"./Meta\";",
        "an imported-from shape is re-exported like any other",
    );
    assert_has(
        index,
        "export type { Choice } from \"./Choice\";",
        "and so is a union",
    );
}

/// Nothing generated may contain a tab or a trailing space.
///
/// Not pedantry: the output is committed in somebody else's repository, and a
/// trailing space is the single most common thing a linter or an editor
/// rewrites on save — which would turn every generated file into a permanent
/// diff against `--check`.
#[test]
fn the_output_is_stable_under_the_formatters_it_will_meet() {
    for layout in [Layout::Type, Layout::Manifest] {
        let opts = Options {
            layout,
            barrel: true,
            ..Options::default()
        };
        for (name, text) in emit_directory(&contracts(), &opts) {
            assert!(
                !text.contains('\t'),
                "{}: contains a tab; the output is two-space indented throughout",
                name.display()
            );
            for (n, line) in text.lines().enumerate() {
                assert_eq!(
                    line.trim_end(),
                    line,
                    "{}:{}: trailing whitespace",
                    name.display(),
                    n + 1
                );
            }
            assert!(
                text.ends_with('\n') && !text.ends_with("\n\n"),
                "{}: must end with exactly one newline",
                name.display()
            );
        }
    }
}
