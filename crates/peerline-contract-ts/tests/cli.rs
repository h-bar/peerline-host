//! The binary, and `--check` in particular.
//!
//! `--check` is the half of this tool a build depends on, and its contract is
//! that it **writes nothing, anywhere** — a check that regenerates the tree it
//! is checking always passes. So the tests here assert the absence of writes
//! as carefully as they assert the exit codes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_peerline-contract-ts");

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contracts")
}

/// The collision fixture: two manifests declaring one `Session`. See
/// `tests/contracts-cluster/`'s own prose.
fn cluster() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contracts-cluster")
}

/// A scratch directory of our own, removed first so a previous failed run
/// cannot make this one pass.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "peerline-contract-ts-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("the binary must run")
}

/// Every `.ts` file under `dir`, with its contents, as one comparable value.
fn snapshot(dir: &Path) -> Vec<(String, String)> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = listing
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn generate_then_check_is_clean_and_check_writes_nothing() {
    let out = scratch("clean");
    let dir = out.to_string_lossy().into_owned();
    let src = contracts().to_string_lossy().into_owned();

    let generated = run(&[&src, &dir]);
    assert!(
        generated.status.success(),
        "generating failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let before = snapshot(&out);
    assert!(!before.is_empty(), "nothing was written");

    let checked = run(&["--check", &src, &dir]);
    assert!(
        checked.status.success(),
        "a freshly generated tree must pass --check: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(
        before,
        snapshot(&out),
        "--check must not write, and must not touch what is there"
    );

    let _ = std::fs::remove_dir_all(&out);
}

/// A file whose contents have drifted: the first of the three failures
/// `--check` looks for.
#[test]
fn check_fails_on_drift_and_names_the_file_and_the_line() {
    let out = scratch("drift");
    let dir = out.to_string_lossy().into_owned();
    let src = contracts().to_string_lossy().into_owned();
    assert!(run(&[&src, &dir]).status.success());

    let victim = out.join("Backend.ts");
    let original = std::fs::read_to_string(&victim).unwrap();
    std::fs::write(&victim, original.replace("\"codex\"", "\"codexx\"")).unwrap();

    let checked = run(&["--check", &src, &dir]);
    assert!(!checked.status.success(), "drift must fail the check");
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(stderr.contains("Backend.ts"), "{stderr}");
    assert!(stderr.contains("differs"), "{stderr}");
    assert!(
        stderr.contains("codexx") && stderr.contains("Run the same command without `--check`"),
        "the report should show the offending line and say what to do: {stderr}"
    );
    // Still no writes, even on the failing path.
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        original.replace("\"codex\"", "\"codexx\""),
        "--check must not repair what it found"
    );

    let _ = std::fs::remove_dir_all(&out);
}

/// A file that is committed and no longer generated — the failure a diff of
/// contents alone cannot see, and the way a renamed type leaves its old
/// binding behind for somebody to import.
#[test]
fn check_fails_on_a_file_that_is_no_longer_generated() {
    let out = scratch("stale");
    let dir = out.to_string_lossy().into_owned();
    let src = contracts().to_string_lossy().into_owned();
    assert!(run(&[&src, &dir]).status.success());

    std::fs::write(out.join("Ghost.ts"), "export type Ghost = string;\n").unwrap();
    // Something that is not ours is not ours to call stale.
    std::fs::write(out.join("package.json"), "{}\n").unwrap();

    let checked = run(&["--check", &src, &dir]);
    assert!(!checked.status.success());
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(stderr.contains("Ghost.ts"), "{stderr}");
    assert!(stderr.contains("no longer generated"), "{stderr}");
    assert!(
        !stderr.contains("package.json"),
        "a hand-written file in the output directory is not this tool's to \
         judge: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&out);
}

/// A missing file, i.e. a shape that has been added to the manifest and never
/// regenerated.
#[test]
fn check_fails_when_a_generated_file_is_absent() {
    let out = scratch("absent");
    let dir = out.to_string_lossy().into_owned();
    let src = contracts().to_string_lossy().into_owned();
    assert!(run(&[&src, &dir]).status.success());
    std::fs::remove_file(out.join("Item.ts")).unwrap();

    let checked = run(&["--check", &src, &dir]);
    assert!(!checked.status.success());
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(
        stderr.contains("Item.ts") && stderr.contains("not committed"),
        "{stderr}"
    );

    let _ = std::fs::remove_dir_all(&out);
}

/// `--check` against a directory that does not exist is a failure with a
/// report, not a crash: it is what a first run in CI looks like.
#[test]
fn check_against_nothing_reports_every_file_as_missing() {
    let out = scratch("empty");
    let checked = run(&[
        "--check",
        &contracts().to_string_lossy(),
        &out.to_string_lossy(),
    ]);
    assert!(!checked.status.success());
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(stderr.contains("not committed"), "{stderr}");
    assert!(
        !out.exists(),
        "--check must not create the directory either"
    );
}

/// The folder layout round-trips through `--check` like the flat ones — and
/// the three failures `--check` looks for reach inside the folders.
#[test]
fn folder_layout_generates_checks_clean_and_sees_inside_the_folders() {
    let out = scratch("folder");
    let dir = out.to_string_lossy().into_owned();
    let src = cluster().to_string_lossy().into_owned();

    let generated = run(&["--layout", "folder", "--barrel", &src, &dir]);
    assert!(
        generated.status.success(),
        "generating failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    assert!(
        out.join("pipe.rpc.v1/Session.ts").is_file()
            && out.join("pipe.events.v1/Session.ts").is_file(),
        "the two Sessions land in their own folders"
    );
    let checked = run(&["--check", "--layout", "folder", "--barrel", &src, &dir]);
    assert!(
        checked.status.success(),
        "a freshly generated tree must pass --check: {}",
        String::from_utf8_lossy(&checked.stderr)
    );

    // Drift inside a folder is found and named by its relative path.
    let victim = out.join("pipe.vocab.v1/Meta.ts");
    let original = std::fs::read_to_string(&victim).unwrap();
    std::fs::write(&victim, original.replace("timestamp_ms", "timestamp")).unwrap();
    let checked = run(&["--check", "--layout", "folder", "--barrel", &src, &dir]);
    assert!(!checked.status.success());
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(stderr.contains("pipe.vocab.v1/Meta.ts"), "{stderr}");
    std::fs::write(&victim, original).unwrap();

    // A stale file inside a manifest-named folder is ours to call stale; a
    // folder that is not named like a manifest is not ours to look into.
    std::fs::write(
        out.join("pipe.vocab.v1/Ghost.ts"),
        "export type Ghost = string;\n",
    )
    .unwrap();
    std::fs::create_dir_all(out.join("src")).unwrap();
    std::fs::write(
        out.join("src/handwritten.ts"),
        "export type Mine = string;\n",
    )
    .unwrap();
    let checked = run(&["--check", "--layout", "folder", "--barrel", &src, &dir]);
    assert!(!checked.status.success());
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(
        stderr.contains("pipe.vocab.v1/Ghost.ts") && stderr.contains("no longer generated"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("handwritten.ts"),
        "a hand-written folder is not this tool's to judge: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&out);
}

/// A directory two of whose manifests declare one type name is refused under
/// the default layout, as a usage-class failure that names the ways out.
#[test]
fn a_type_name_collision_is_a_pointed_usage_error() {
    let out = scratch("collide");
    let result = run(&[&cluster().to_string_lossy(), &out.to_string_lossy()]);
    assert_eq!(
        result.status.code(),
        Some(2),
        "a directory the layout cannot write is exit 2, not a silent overwrite"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    for needle in [
        "`Session`",
        "`pipe.events/1`",
        "`pipe.rpc/1`",
        "--layout folder",
        "--only",
    ] {
        assert!(stderr.contains(needle), "{stderr}");
    }
    assert!(!out.exists(), "nothing may be written on the failing path");
}

/// `--only` narrows the run, pulls imports in, and composes with `--check` —
/// including against a tree that also holds contracts the run did not name.
#[test]
fn only_generates_the_subset_and_checks_it_inside_a_fuller_tree() {
    let out = scratch("only");
    let dir = out.to_string_lossy().into_owned();
    let src = cluster().to_string_lossy().into_owned();

    let generated = run(&["--layout", "folder", "--only", "pipe.rpc/1", &src, &dir]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    assert!(
        out.join("pipe.rpc.v1/Session.ts").is_file() && out.join("pipe.vocab.v1/Meta.ts").is_file(),
        "the pinned contract and its transitive import"
    );
    assert!(
        !out.join("pipe.rpc.v2").exists() && !out.join("pipe.events.v1").exists(),
        "nothing that was not named or imported"
    );

    // Regenerate the whole directory over it, then check the subset: the
    // files the run did not generate are not its to call stale.
    assert!(run(&["--layout", "folder", &src, &dir]).status.success());
    let checked = run(&[
        "--check",
        "--layout",
        "folder",
        "--only",
        "pipe.rpc/1",
        &src,
        &dir,
    ]);
    assert!(
        checked.status.success(),
        "a subset check must not judge the rest of the tree: {}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let _ = std::fs::remove_dir_all(&out);
}

/// An `--only` that names nothing the directory holds is a usage error whose
/// message lists what it does hold.
#[test]
fn only_an_unknown_contract_is_a_usage_error_listing_the_directory() {
    let out = scratch("only-unknown");
    let result = run(&[
        "--only",
        "pipe.rcp",
        &cluster().to_string_lossy(),
        &out.to_string_lossy(),
    ]);
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    for needle in ["`pipe.rcp`", "`pipe.rpc/1`", "`pipe.vocab/1`"] {
        assert!(stderr.contains(needle), "{stderr}");
    }
    assert!(!out.exists());
}

/// A bad manifest is a usage-class failure, distinguishable from drift by its
/// exit code — so a build script can tell "the contract is broken" from "the
/// bindings are stale".
#[test]
fn a_broken_contracts_directory_exits_differently_from_drift() {
    let out = scratch("broken");
    let checked = run(&["/nonexistent/contracts", &out.to_string_lossy()]);
    assert_eq!(
        checked.status.code(),
        Some(2),
        "a directory that cannot be read is exit 2, not exit 1"
    );
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(stderr.contains("peerline-contract-ts:"), "{stderr}");
}

#[test]
fn the_options_are_rejected_when_misspelled() {
    let out = scratch("opts");
    for bad in [
        vec!["--layout", "files"],
        vec!["--wide-integers", "int64"],
        vec!["--defaults", "maybe"],
        vec!["--only", "demo.every/one"],
        vec!["--nonsense"],
    ] {
        let mut args = bad.clone();
        let src = contracts().to_string_lossy().into_owned();
        let dst = out.to_string_lossy().into_owned();
        args.push(&src);
        args.push(&dst);
        let result = run(&args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "`{bad:?}` should be a usage error, not a silent default"
        );
    }
    assert!(!out.exists(), "nothing should have been written");
}

#[test]
fn help_is_help_and_succeeds() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--check"), "{stdout}");
    assert!(stdout.contains("<contracts-dir> <out-dir>"), "{stdout}");
    assert!(stdout.contains("--only"), "{stdout}");
    assert!(stdout.contains("folder"), "{stdout}");
    assert!(stdout.contains("--ops"), "{stdout}");
}

/// The fixture for the forms the format grew later, which is also the one
/// with ops and error codes worth emitting constants for.
fn shapes() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contracts-shapes")
}

/// `--ops` is off by default and additive when on, at the level a build
/// actually sees it: the same command without the flag writes a tree, and
/// with it writes that tree plus one file per contract.
#[test]
fn ops_is_off_by_default_and_adds_one_file_per_contract() {
    let out = scratch("ops");
    let dir = out.to_string_lossy().into_owned();
    let src = shapes().to_string_lossy().into_owned();

    assert!(run(&[&src, &dir]).status.success());
    let without = snapshot(&out);
    assert!(
        !out.join("demo.shapes.v1.ops.ts").exists(),
        "the flag is off by default, so a tree generated without it is unchanged"
    );

    let _ = std::fs::remove_dir_all(&out);
    assert!(run(&["--ops", &src, &dir]).status.success());
    let with = snapshot(&out);
    for (name, text) in &without {
        assert!(
            with.contains(&(name.clone(), text.clone())),
            "`{name}` must be byte-identical with the flag on"
        );
    }
    assert_eq!(
        with.len(),
        without.len() + 1,
        "one contract with ops, one vocabulary without: exactly one new file"
    );
    let ops = std::fs::read_to_string(out.join("demo.shapes.v1.ops.ts")).unwrap();
    assert!(
        ops.contains(r#"export const OP_START = "demo.shapes.start";"#),
        "{ops}"
    );
    assert!(
        ops.contains("export const ERR_NO_SUCH_HOST = 3101;"),
        "{ops}"
    );

    let _ = std::fs::remove_dir_all(&out);
}

/// It round-trips through `--check` like everything else, and — this is the
/// part that needed no new rule — an ops file left behind by a run that
/// stopped passing the flag is reported as no longer generated, because it
/// sits where the staleness rule already looks.
#[test]
fn an_ops_file_is_checked_and_goes_stale_like_any_other() {
    let out = scratch("ops-check");
    let dir = out.to_string_lossy().into_owned();
    let src = shapes().to_string_lossy().into_owned();

    assert!(run(&["--ops", &src, &dir]).status.success());
    let checked = run(&["--check", "--ops", &src, &dir]);
    assert!(
        checked.status.success(),
        "a freshly generated tree must pass --check: {}",
        String::from_utf8_lossy(&checked.stderr)
    );

    // Drop the flag: the file is now this tool's to call stale.
    let checked = run(&["--check", &src, &dir]);
    assert!(!checked.status.success());
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(
        stderr.contains("demo.shapes.v1.ops.ts") && stderr.contains("no longer generated"),
        "{stderr}"
    );

    let _ = std::fs::remove_dir_all(&out);
}

/// Inside a folder, under the folder layout — the other of the two places the
/// staleness rule already owns.
#[test]
fn an_ops_file_inside_a_folder_is_checked_too() {
    let out = scratch("ops-folder");
    let dir = out.to_string_lossy().into_owned();
    let src = shapes().to_string_lossy().into_owned();

    assert!(
        run(&["--layout", "folder", "--ops", &src, &dir])
            .status
            .success()
    );
    assert!(
        out.join("demo.shapes.v1/ops.ts").is_file(),
        "the manifest owns a directory here, so its constants live in it"
    );
    assert!(
        !out.join("demo.shared.v1/ops.ts").exists(),
        "a vocabulary has nothing to put in one"
    );

    let victim = out.join("demo.shapes.v1/ops.ts");
    let original = std::fs::read_to_string(&victim).unwrap();
    std::fs::write(&victim, original.replace("3101", "3199")).unwrap();
    let checked = run(&["--check", "--layout", "folder", "--ops", &src, &dir]);
    assert!(!checked.status.success());
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(stderr.contains("demo.shapes.v1/ops.ts"), "{stderr}");

    let _ = std::fs::remove_dir_all(&out);
}
