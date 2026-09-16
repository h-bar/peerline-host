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
use peerline_contract_ts::{
    ContractSelector, DefaultFields, Layout, Options, WideIntegers, emit_directory,
};

fn contracts() -> Directory {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contracts");
    Directory::load(&dir).expect("the fixture contracts directory must parse and validate")
}

/// The collision fixture: two contracts that each declare a `Session`, one
/// of them at two versions, all importing one vocabulary. The smallest
/// honest example of a directory the flat per-type layout cannot write —
/// see the manifests' own prose.
fn cluster() -> Directory {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contracts-cluster");
    Directory::load(&dir).expect("the cluster fixture directory must parse and validate")
}

/// The second checklist: the forms the format grew after the first one was
/// written — maps, newtypes, a field that is both absent-able and defaulted,
/// and a contract claiming a band of the error-code space.
///
/// A directory of its own rather than more types in `tests/contracts`, and
/// deliberately. Every golden the first checklist produces has to stay
/// byte-for-byte what it was: a generator whose new features moved its old
/// output is a generator nobody can adopt one feature at a time, and the
/// cheapest way to prove that is to leave the old fixture alone entirely.
fn shapes() -> Directory {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contracts-shapes");
    Directory::load(&dir).expect("the shapes fixture directory must parse and validate")
}

/// [`emit_directory`] where the fixtures make failure impossible, which is
/// every call in this suite that is not about `EmitError` itself.
fn emitted(dir: &Directory, opts: &Options) -> BTreeMap<PathBuf, String> {
    emit_directory(dir, opts).expect("these fixtures emit cleanly under these options")
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
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("creating a golden subdirectory");
            }
            std::fs::write(path, text).expect("writing a golden file");
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

/// Every committed `.ts` under `root`, as paths relative to it — recursing,
/// because the folder layout's goldens have subdirectories.
fn committed_ts(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) {
        let Ok(listing) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in listing.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.extension().is_some_and(|x| x == "ts")
                && let Ok(relative) = path.strip_prefix(base)
            {
                out.push(relative.to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
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
    check_tree("type", &emitted(&contracts(), &opts));
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
    check_tree("manifest", &emitted(&contracts(), &opts));
}

/// The cluster fixture, one folder per manifest with both barrels. This is
/// the layout's whole story on one tree: two `Session.ts` files that cannot
/// collide, an import that crosses folders, and a root barrel whose
/// namespaces carry a version exactly where two versions demand it.
#[test]
fn one_folder_per_manifest() {
    let opts = Options {
        layout: Layout::Folder,
        barrel: true,
        ..Options::default()
    };
    check_tree("folder", &emitted(&cluster(), &opts));
}

/// The second checklist, one file per type, with the barrel and the ops file.
/// Maps, newtypes and the absent-able default all in one readable tree.
#[test]
fn the_newer_forms_one_file_per_type() {
    let opts = Options {
        barrel: true,
        ops: true,
        ..Options::default()
    };
    check_tree("shapes", &emitted(&shapes(), &opts));
}

/// The same, one folder per manifest — which is where the ops file lands
/// somewhere different, and where an imported newtype has to be reached
/// across folders.
#[test]
fn the_newer_forms_one_folder_per_manifest() {
    let opts = Options {
        layout: Layout::Folder,
        barrel: true,
        ops: true,
        ..Options::default()
    };
    check_tree("shapes-folder", &emitted(&shapes(), &opts));
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
    emitted(&contracts(), &Options::default())
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
    let files = emitted(&contracts(), &opts);
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
    let files = emitted(&contracts(), &opts);
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
    let files = emitted(&contracts(), &opts);
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
    let files = emitted(&contracts(), &opts);
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

/// A folder file is the flat per-type file, except where an import points.
/// That is the layout's compatibility claim, so it is asserted as an
/// equality rather than left to the goldens.
#[test]
fn a_folder_file_is_the_type_layout_file_with_rebased_imports() {
    let flat = emitted(&contracts(), &Options::default());
    let opts = Options {
        layout: Layout::Folder,
        ..Options::default()
    };
    let folder = emitted(&contracts(), &opts);
    assert_eq!(
        flat[&PathBuf::from("Backend.ts")],
        folder[&PathBuf::from("demo.every.v1/Backend.ts")],
        "a file with no imports is byte-identical"
    );
    assert_eq!(
        flat[&PathBuf::from("Event.ts")].replace("\"./Meta\"", "\"../demo.vocab.v1/Meta\""),
        folder[&PathBuf::from("demo.every.v1/Event.ts")],
        "the only difference is where a cross-manifest import points"
    );
}

/// Inside one folder, both kinds of import: a sibling stays `./`, and a
/// vocabulary shape is reached through the other manifest's folder.
#[test]
fn a_folder_import_crosses_into_the_declaring_manifests_folder() {
    let opts = Options {
        layout: Layout::Folder,
        ..Options::default()
    };
    let files = emitted(&cluster(), &opts);
    let ts = &files[&PathBuf::from("pipe.events.v1/Frame.ts")];
    assert_has(
        ts,
        "import type { Meta } from \"../pipe.vocab.v1/Meta\";",
        "an imported shape lives one folder up and back down",
    );
    assert_has(
        ts,
        "import type { Session } from \"./Session\";",
        "a sibling stays where the flat layout would put it",
    );
    // And the two Sessions really are two files with two owners.
    let rpc = &files[&PathBuf::from("pipe.rpc.v1/Session.ts")];
    let events = &files[&PathBuf::from("pipe.events.v1/Session.ts")];
    assert_has(rpc, "`pipe.rpc/1`", "each banner names its own manifest");
    assert_has(
        events,
        "`pipe.events/1`",
        "each banner names its own manifest",
    );
    assert_has(
        events,
        "active: boolean;",
        "and the shapes genuinely differ",
    );
    assert!(!rpc.contains("active"), "{rpc}");
}

/// The two barrels under the folder layout: one per folder, and a root of
/// namespace re-exports — with `V<n>` on a namespace exactly where the
/// directory holds two versions of one contract.
#[test]
fn the_folder_barrels_namespace_each_manifest_and_suffix_only_where_versions_collide() {
    let opts = Options {
        layout: Layout::Folder,
        barrel: true,
        ..Options::default()
    };
    let files = emitted(&cluster(), &opts);

    let folder_index = &files[&PathBuf::from("pipe.events.v1/index.ts")];
    assert_has(
        folder_index,
        "export type * from \"./Frame\";",
        "a folder's barrel re-exports its own types",
    );
    assert_has(
        folder_index,
        "export type * from \"./Session\";",
        "all of them",
    );

    let root = &files[&PathBuf::from("index.ts")];
    assert_has(
        root,
        "export type * as PipeRpcV1 from \"./pipe.rpc.v1/index\";",
        "two versions of pipe.rpc, so each namespace carries its version",
    );
    assert_has(
        root,
        "export type * as PipeRpcV2 from \"./pipe.rpc.v2/index\";",
        "both of them",
    );
    assert_has(
        root,
        "export type * as PipeVocab from \"./pipe.vocab.v1/index\";",
        "one version of pipe.vocab, so no suffix distinguishing nothing",
    );
    assert!(
        !root.contains("PipeVocabV1"),
        "the suffix is only for a contract the directory holds twice:\n{root}"
    );
}

/// The flat per-type layout refuses a directory two of whose manifests
/// declare one name — the alternative is that whichever sorts last silently
/// wins. The message names the type, both manifests, and both ways out.
#[test]
fn a_shared_type_name_under_the_flat_layout_is_an_error_naming_both_manifests() {
    let err = emit_directory(&cluster(), &Options::default())
        .expect_err("`Session` is declared by two manifests");
    let message = err.to_string();
    for needle in [
        "`Session`",
        "`pipe.events/1`",
        "`pipe.rpc/1`",
        "--layout folder",
        "--only",
    ] {
        assert!(
            message.contains(needle),
            "the error should carry `{needle}`: {message}"
        );
    }
}

/// `only` narrows to the named contract, and always pulls in what it
/// imports: an emitted file names its imported shapes by path, so a subset
/// that dangled would be a lie the type-checker catches only downstream.
#[test]
fn only_narrows_and_always_pulls_transitive_imports() {
    let vocab_only = Options {
        only: Some(vec![ContractSelector {
            contract: "demo.vocab".into(),
            version: None,
        }]),
        ..Options::default()
    };
    let files = emitted(&contracts(), &vocab_only);
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        [&PathBuf::from("Direction.ts"), &PathBuf::from("Meta.ts")],
        "a vocabulary imports nothing, so it alone is the subset"
    );

    let every = Options {
        only: Some(vec![ContractSelector {
            contract: "demo.every".into(),
            version: None,
        }]),
        ..Options::default()
    };
    let files = emitted(&contracts(), &every);
    assert!(
        files.contains_key(&PathBuf::from("Meta.ts"))
            && files.contains_key(&PathBuf::from("Direction.ts")),
        "demo.every imports the vocabulary, so the vocabulary comes along"
    );
    assert_eq!(
        files,
        emitted(&contracts(), &Options::default()),
        "naming the one contract that imports the rest is the whole directory"
    );
}

/// A pinned version selects one manifest — which is also the narrowing that
/// makes the flat layout legal again for a directory it would refuse whole.
#[test]
fn only_pinned_to_a_version_narrows_past_the_collision() {
    let opts = Options {
        only: Some(vec![ContractSelector {
            contract: "pipe.rpc".into(),
            version: Some(1),
        }]),
        ..Options::default()
    };
    let files = emitted(&cluster(), &opts);
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        [&PathBuf::from("Meta.ts"), &PathBuf::from("Session.ts")],
        "pipe.rpc/1 plus its one import, and neither v2 nor pipe.events"
    );
    assert_has(
        &files[&PathBuf::from("Session.ts")],
        "`pipe.rpc/1`",
        "the surviving Session is the pinned version's",
    );
    assert!(
        !files[&PathBuf::from("Session.ts")].contains("title:"),
        "v2's field must not appear"
    );
}

/// A name or a version the directory does not hold is an error that lists
/// what it does, so the message is also the answer.
#[test]
fn only_an_unknown_selection_lists_what_the_directory_holds() {
    let unknown_name = Options {
        only: Some(vec![ContractSelector {
            contract: "pipe.rcp".into(),
            version: None,
        }]),
        ..Options::default()
    };
    let message = emit_directory(&cluster(), &unknown_name)
        .expect_err("no such contract")
        .to_string();
    for needle in [
        "`pipe.rcp`",
        "`pipe.events/1`",
        "`pipe.rpc/1`",
        "`pipe.vocab/1`",
    ] {
        assert!(message.contains(needle), "{message}");
    }

    let unknown_version = Options {
        only: Some(vec![ContractSelector {
            contract: "pipe.rpc".into(),
            version: Some(3),
        }]),
        ..Options::default()
    };
    let message = emit_directory(&cluster(), &unknown_version)
        .expect_err("no such version")
        .to_string();
    for needle in ["`pipe.rpc`", "version 3", "version 1 and version 2"] {
        assert!(message.contains(needle), "{message}");
    }
}

// ---------------------------------------------------------------------------
// The newer forms, asserted by name.
// ---------------------------------------------------------------------------

fn shapes_file(name: &str) -> String {
    let files = emitted(
        &shapes(),
        &Options {
            ops: true,
            ..Options::default()
        },
    );
    files
        .get(&PathBuf::from(name))
        .unwrap_or_else(|| panic!("`{name}` was not generated; files: {:?}", files.keys()))
        .clone()
}

/// A map is `Record<string, T>`, and the key type is `string` because a JSON
/// object's keys are strings — there is no key-type keyword in the format and
/// nothing one could say with it.
#[test]
fn a_map_is_a_record_keyed_by_string() {
    let ts = shapes_file("Environment.ts");
    assert_has(&ts, "vars: Record<string, string>;", "a map of a scalar");
    assert_has(
        &ts,
        "layers: Record<string, Array<string>>;",
        "a map of a list, which is the two composites nesting",
    );
    assert_has(
        &ts,
        "by_host: Record<string, HostId>;",
        "a map whose values are a named shape, which is therefore imported",
    );
    assert_has(
        &ts,
        "import type { HostId } from \"./HostId\";",
        "a map's value schema is collected like any other reference",
    );
}

/// `Record<string, never>` belongs to an *empty struct* — a shape with no
/// keys — and never arises from a map. A map with no entries is a value, not
/// a type, and its type is still `Record<string, T>`.
#[test]
fn an_empty_map_is_a_value_and_never_the_empty_record_type() {
    let opts = Options {
        ops: true,
        ..Options::default()
    };
    for (_, text) in emitted(&shapes(), &opts) {
        assert!(
            !text.contains("Record<string, never>"),
            "nothing in this fixture is an empty struct, so nothing should be the \
             empty-object type:\n{text}"
        );
    }
    // And the empty map that *is* here is a default value in a doc comment.
    assert_has(
        &shapes_file("Environment.ts"),
        "Absent means `{}`.",
        "an empty map is a value",
    );
}

/// A newtype is a plain alias, not a branded type — because every value of a
/// generated type arrives from `JSON.parse`, and a parsed string is not
/// assignable to a branded one.
#[test]
fn a_newtype_is_a_plain_alias_and_never_a_brand() {
    assert_has(
        &shapes_file("HostId.ts"),
        "export type HostId = string;",
        "the inner shape, under the manifest's name",
    );
    assert_has(
        &shapes_file("Millis.ts"),
        "export type Millis = number;",
        "a newtype over a safe integer is that integer's TypeScript",
    );
    assert_has(
        &shapes_file("Labels.ts"),
        "export type Labels = Record<string, string>;",
        "a newtype over a composite is that composite",
    );
    for name in ["HostId.ts", "Millis.ts", "Labels.ts"] {
        let ts = shapes_file(name);
        assert!(
            !ts.contains("__brand") && !ts.contains("unique symbol"),
            "a brand would break assignability from parsed JSON, which is where \
             every value comes from:\n{ts}"
        );
    }
    // The prose is what carries the distinction to a TypeScript reader, since
    // the type cannot.
    assert_has(
        &shapes_file("HostId.ts"),
        "Which host, in the coordinator's own namespace.",
        "the name means something even where the type does not distinguish it",
    );
}

/// A newtype declared in another manifest is imported like any other shape,
/// and under the folder layout it is reached across folders.
#[test]
fn an_imported_newtype_is_imported_like_any_other_shape() {
    assert_has(
        &shapes_file("Meta.ts"),
        "import type { TraceId } from \"./TraceId\";",
        "under the flat layout every reference is its own file",
    );
    assert_has(
        &shapes_file("StartArgs.ts"),
        "trace?: TraceId;",
        "and it is used by name",
    );

    let opts = Options {
        layout: Layout::Folder,
        ops: true,
        ..Options::default()
    };
    let files = emitted(&shapes(), &opts);
    assert_has(
        &files[&PathBuf::from("demo.shapes.v1/StartArgs.ts")],
        "import type { TraceId } from \"../demo.shared.v1/TraceId\";",
        "an imported newtype lives in the declaring manifest's own folder",
    );
}

/// The pair — `optional` with a `default` — is `key?: T` with the value in
/// the prose, plus the one line the type cannot carry: that a producer really
/// does omit the key.
#[test]
fn an_absent_able_default_is_optional_and_says_both_halves() {
    let ts = shapes_file("Diagnostic.ts");
    assert_has(&ts, "dropped?: number;", "absence is legal on this wire");
    assert_has(&ts, "Absent means `0`.", "and it carries a value");
    assert_has(
        &ts,
        "A writer omits the key when the value is that, so an absent key is something \
         this contract's own producer emits.",
        "the direction TypeScript's one type for both directions cannot express",
    );
    // Through every composite and every named form the manifest has.
    assert_has(&ts, "output?: Array<string>;", "a list");
    assert_has(&ts, "Absent means `[]`.", "defaulting to the empty list");
    assert_has(&ts, "labels?: Labels;", "a newtype over a map");
    assert_has(&ts, "mode?: Mode;", "an enum");
    assert_has(
        &ts,
        "Absent means `\"best-effort\"`.",
        "whose default is the wire spelling",
    );
    assert_has(&ts, "host?: HostId;", "a newtype over a string");
    assert_has(
        &ts,
        "Absent means `\"local\"`.",
        "whose default is checked through to what it wraps",
    );
}

/// `--defaults required` reproduces the `ts_rs` route for a *plain* default,
/// and has nothing to say about the pair: a producer of a pair field really
/// does omit the key, so `key: T` there would not be a conservative reading
/// of an ambiguous case — it would be false.
#[test]
fn the_defaults_option_does_not_reach_the_absent_able_pair() {
    let opts = Options {
        defaults: DefaultFields::Required,
        ..Options::default()
    };
    let files = emitted(&shapes(), &opts);
    let ts = &files[&PathBuf::from("Diagnostic.ts")];
    assert_has(
        ts,
        "dropped?: number;",
        "still absent-able under the option",
    );
    assert_has(ts, "label?: string;", "all of them");
    assert!(
        !ts.contains("dropped: number;"),
        "the option must not turn a key a producer omits into one it writes:\n{ts}"
    );

    // And the option still does what it is for, on the fixture that has a
    // plain default.
    let flat = emitted(&contracts(), &opts);
    assert_has(
        &flat[&PathBuf::from("States.ts")],
        "include_history: boolean;",
        "a plainly defaulted field is still emitted the ts-rs way",
    );
}

/// An externally tagged arm may carry a map, which is the case internal
/// tagging cannot express: the keys are the contract's data and one of them
/// could be the discriminator.
#[test]
fn an_externally_tagged_arm_may_carry_a_map() {
    let ts = shapes_file("Selector.ts");
    assert_has(
        &ts,
        "\"by-label\": Labels;",
        "the arm's wire name is the key and the map is its whole value",
    );
    assert_has(&ts, "\"by-host\": Array<HostId>;", "and a list beside it");
    assert_has(&ts, "| \"everything\";", "and a bare string for a unit arm");
}

// ---------------------------------------------------------------------------
// `--ops`: opt-in constants, in a file of their own
// ---------------------------------------------------------------------------

/// **The compatibility claim, asserted as an equality rather than argued.**
/// With the flag off the output is exactly what it has always been; with it
/// on, every file that was there before is byte-identical and the only
/// difference is files that did not exist.
#[test]
fn the_ops_flag_only_ever_adds_files() {
    for dir in [contracts(), cluster(), shapes()] {
        for layout in [Layout::Manifest, Layout::Folder] {
            for barrel in [false, true] {
                let without = Options {
                    layout,
                    barrel,
                    ..Options::default()
                };
                let with = Options {
                    ops: true,
                    ..without.clone()
                };
                let before = emitted(&dir, &without);
                let after = emitted(&dir, &with);
                for (name, text) in &before {
                    assert_eq!(
                        after.get(name),
                        Some(text),
                        "{}: `--ops` must not change a file that was already generated",
                        name.display()
                    );
                }
                assert!(
                    after.len() > before.len(),
                    "and it must actually add something for a directory with ops"
                );
                for name in after.keys() {
                    assert!(
                        before.contains_key(name) || name.to_string_lossy().contains("ops"),
                        "{}: the only new files may be ops files",
                        name.display()
                    );
                }
            }
        }
    }
}

/// Where the file lands under each layout: beside the types where the layout
/// is flat, and inside the manifest's own folder where it is not.
#[test]
fn an_ops_file_lands_where_the_layout_already_puts_per_manifest_things() {
    for (layout, expected) in [
        (Layout::Type, "demo.shapes.v1.ops.ts"),
        (Layout::Manifest, "demo.shapes.v1.ops.ts"),
        (Layout::Folder, "demo.shapes.v1/ops.ts"),
    ] {
        let opts = Options {
            layout,
            ops: true,
            ..Options::default()
        };
        let files = emitted(&shapes(), &opts);
        assert!(
            files.contains_key(&PathBuf::from(expected)),
            "expected `{expected}`, got {:?}",
            files.keys()
        );
    }
}

/// Nobody produces a vocabulary, so it declares no ops and raises no errors —
/// and a file holding two empty lists would be a file with nothing in it.
#[test]
fn a_vocabulary_gets_no_ops_file() {
    let opts = Options {
        ops: true,
        ..Options::default()
    };
    let files = emitted(&shapes(), &opts);
    assert!(
        !files.contains_key(&PathBuf::from("demo.shared.v1.ops.ts")),
        "a vocabulary has nothing to put in one: {:?}",
        files.keys()
    );
    assert!(files.contains_key(&PathBuf::from("demo.shapes.v1.ops.ts")));
}

/// The constants are the wire spellings, under the same names the Rust
/// generator produces — a constant names one wire string, and two generators
/// giving it two names would put the work of knowing they are the same thing
/// back on the reader.
#[test]
fn the_ops_file_carries_wire_spellings_under_the_rust_generators_names() {
    let ts = shapes_file("demo.shapes.v1.ops.ts");
    assert_has(
        &ts,
        "export const OP_START = \"demo.shapes.start\";",
        "the op name as it travels, not the short one",
    );
    assert_has(
        &ts,
        "export const OP_ENVIRONMENT_READ = \"demo.shapes.environment.read\";",
        "a dotted op name becomes one underscored constant",
    );
    assert_has(
        &ts,
        "export const ERR_NO_SUCH_HOST = 3101;",
        "an error code is the number a consumer matches on",
    );
    assert_has(
        &ts,
        "Start one instance and answer when it is ready.",
        "with the manifest's prose above it",
    );
    assert_has(
        &ts,
        "Kind: stream.",
        "plus the one line the manifest cannot write: which call to make",
    );
    assert_has(
        &ts,
        "Carried in the error's `data` as `Diagnostic`.",
        "and what a matched code lets a consumer decode",
    );
}

/// No barrel re-exports it. The barrels are `export type *`, which is erased
/// at compile time; an ops file holds runtime values, and folding it in would
/// turn every barrel from an erased import into an emitted one.
#[test]
fn no_barrel_re_exports_the_ops_file() {
    for layout in [Layout::Type, Layout::Manifest, Layout::Folder] {
        let opts = Options {
            layout,
            barrel: true,
            ops: true,
            ..Options::default()
        };
        let files = emitted(&shapes(), &opts);
        for (name, text) in &files {
            if !name.to_string_lossy().ends_with("index.ts") {
                continue;
            }
            assert!(
                !text.contains("ops"),
                "{}: a type-only barrel cannot carry a value, and a value-carrying \
                 one would add a runtime import to every consumer:\n{text}",
                name.display()
            );
        }
    }
}

/// `--only` governs ops files exactly as it governs types, because a subset
/// selects manifests and an ops file is a per-manifest artefact.
#[test]
fn only_narrows_the_ops_files_with_everything_else() {
    let opts = Options {
        ops: true,
        only: Some(vec![ContractSelector {
            contract: "demo.shared".into(),
            version: None,
        }]),
        ..Options::default()
    };
    let files = emitted(&shapes(), &opts);
    assert!(
        files.keys().all(|k| !k.to_string_lossy().contains("ops")),
        "the vocabulary alone has no ops file: {:?}",
        files.keys()
    );

    let opts = Options {
        ops: true,
        only: Some(vec![ContractSelector {
            contract: "demo.shapes".into(),
            version: Some(1),
        }]),
        ..Options::default()
    };
    let files = emitted(&shapes(), &opts);
    assert!(files.contains_key(&PathBuf::from("demo.shapes.v1.ops.ts")));
}

/// Nothing generated may contain a tab or a trailing space.
///
/// Not pedantry: the output is committed in somebody else's repository, and a
/// trailing space is the single most common thing a linter or an editor
/// rewrites on save — which would turn every generated file into a permanent
/// diff against `--check`.
#[test]
fn the_output_is_stable_under_the_formatters_it_will_meet() {
    let trees = [
        (contracts(), Layout::Type),
        (contracts(), Layout::Manifest),
        (contracts(), Layout::Folder),
        (cluster(), Layout::Manifest),
        (cluster(), Layout::Folder),
        (shapes(), Layout::Type),
        (shapes(), Layout::Manifest),
        (shapes(), Layout::Folder),
    ];
    for (dir, layout) in trees {
        let opts = Options {
            layout,
            barrel: true,
            // The ops files are committed output like any other and meet the
            // same formatters, so they are held to the same rule.
            ops: true,
            ..Options::default()
        };
        for (name, text) in emitted(&dir, &opts) {
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
