//! `peerline-contract-ts <contracts-dir> <out-dir>` — the generator as a
//! build step.
//!
//! # `--check` does not need a scratch directory
//!
//! The house precedent for checking generated output is
//! `peerline-conformance`'s `bindings-check`: export into a `mktemp -d` and
//! `diff -ru` it against the committed tree, so that *checking* never writes
//! into the tree it is checking. The rule is right and this binary keeps it.
//! The scratch directory is not the rule, though — it is a workaround for
//! `ts_rs`, which writes files and returns nothing, so the only way to see its
//! output is to give it somewhere to put it.
//!
//! This generator returns strings. `--check` therefore holds the generated
//! files in memory and compares them against what is on disk, which is the
//! same guarantee with one fewer moving part: nothing is written, nothing has
//! to be cleaned up, no failure can leave a temporary tree behind, and the
//! report can name the first line that differs rather than shelling out to
//! `diff`.
//!
//! It checks three things, because there are three ways a committed tree goes
//! wrong: a file whose contents have drifted, a file the generator now
//! produces that is not committed, and a file that is committed and is no
//! longer generated. The last one is the one a `diff` of contents alone
//! misses, and it is how a renamed type leaves its old binding behind for
//! somebody to import.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use peerline_contract_spec::Directory;
use peerline_contract_ts::{
    ContractSelector, DefaultFields, Layout, Options, WideIntegers, emit_directory,
};

const USAGE: &str = "\
peerline-contract-ts — TypeScript from peerline contract manifests

USAGE:
    peerline-contract-ts [OPTIONS] <contracts-dir> <out-dir>

ARGS:
    <contracts-dir>  A directory of `<contract>.v<version>.json` manifests.
    <out-dir>        Where the `.ts` files go. Created if it is absent.

OPTIONS:
    --check              Generate nothing. Compare what would be generated
                         against <out-dir> and exit non-zero if they differ.
                         Writes nothing, anywhere.
    --layout <type|manifest|folder>
                         One file per named type (default, and what the
                         existing ts-rs bindings are), one per manifest, or a
                         folder per manifest with one file per type inside —
                         the layout for a directory whose manifests share
                         type names, which `type` refuses.
    --only <contract>[/<version>]
                         Generate only this contract — plus its transitive
                         imports, always, because the emitted files import
                         them by path. Bare name means every version the
                         directory holds. Repeatable. Under `--check`, the
                         named contracts' files are checked and the rest of
                         the tree is not judged stale.
    --wide-integers <bigint|number>
                         What `i64` and `u64` become. `bigint` is the default
                         and the truthful one; `number` is an opt-out for a
                         consumer that has checked its own values.
    --defaults <optional|required>
                         Whether a field with a manifest default is emitted
                         `key?: T` (default) or `key: T` (what ts-rs produces).
    --barrel             Also emit `index.ts` re-exporting every type.
    --ops                Also emit one `.ops.ts` per contract, holding its op
                         names and error codes as `export const`s. Off by
                         default: without it the output is byte-for-byte what
                         it has always been. Lands beside the types under the
                         flat layouts (`<contract>.v<n>.ops.ts`) and inside
                         the manifest's own folder under `--layout folder`.
    -h, --help           This.
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("peerline-contract-ts: {message}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    contracts: PathBuf,
    out: PathBuf,
    check: bool,
    opts: Options,
}

fn run() -> Result<ExitCode, String> {
    let Some(args) = parse(std::env::args().skip(1))? else {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };

    let dir = Directory::load(&args.contracts).map_err(|e| e.to_string())?;
    let generated = emit_directory(&dir, &args.opts).map_err(|e| e.to_string())?;
    if generated.is_empty() {
        return Err(format!(
            "`{}` holds manifests that declare no types, so there is nothing to generate",
            args.contracts.display()
        ));
    }

    if args.check {
        return check(&args.out, &generated, &args.opts);
    }

    std::fs::create_dir_all(&args.out)
        .map_err(|e| format!("cannot create `{}`: {e}", args.out.display()))?;
    for (name, text) in &generated {
        let path = args.out.join(name);
        // The folder layout writes into subdirectories the flat layouts
        // never make.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
        }
        std::fs::write(&path, text)
            .map_err(|e| format!("cannot write `{}`: {e}", path.display()))?;
    }
    println!(
        "peerline-contract-ts: wrote {} file{} to `{}`",
        generated.len(),
        plural(generated.len()),
        args.out.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Compare the generated tree against the committed one without writing
/// anything. See the module docs for the three failures this looks for.
fn check(
    out: &Path,
    generated: &std::collections::BTreeMap<PathBuf, String>,
    opts: &Options,
) -> Result<ExitCode, String> {
    let mut drifted: Vec<String> = Vec::new();

    for (name, want) in generated {
        let path = out.join(name);
        match std::fs::read_to_string(&path) {
            Ok(have) if have == *want => {}
            Ok(have) => drifted.push(format!(
                "  {}: differs, {}",
                name.display(),
                first_difference(&have, want)
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                drifted.push(format!("  {}: not committed", name.display()));
            }
            Err(e) => return Err(format!("cannot read `{}`: {e}", path.display())),
        }
    }

    // A file that is committed and no longer generated. Only `.ts` files are
    // considered ours: a hand-written `package.json` or a `tsconfig.json`
    // sitting in the output directory is not this generator's to call stale.
    //
    // Skipped under `--only`, and not reluctantly: a subset run generates
    // files for some contracts, so a committed file it did not generate is
    // either stale or belongs to a contract it was not asked about, and
    // nothing in a subset can tell those apart. The full run is the one that
    // can, and the one whose job it is.
    if opts.only.is_none() {
        for name in committed_ts(out, opts.layout)?.difference(&generated.keys().cloned().collect())
        {
            drifted.push(format!(
                "  {}: committed but no longer generated",
                name.display()
            ));
        }
    }

    if drifted.is_empty() {
        println!(
            "peerline-contract-ts: `{}` matches the contracts directory ({} file{})",
            out.display(),
            generated.len(),
            plural(generated.len())
        );
        return Ok(ExitCode::SUCCESS);
    }
    drifted.sort();
    eprintln!(
        "peerline-contract-ts: `{}` has drifted from the contracts directory.",
        out.display()
    );
    for line in &drifted {
        eprintln!("{line}");
    }
    eprintln!("Run the same command without `--check` to regenerate it.");
    Ok(ExitCode::FAILURE)
}

/// Every committed `.ts` file that is this generator's to judge, as paths
/// relative to `out`.
///
/// The flat layouts own the top level and nothing below it. The folder layout
/// also owns one level down — but only inside directories named the way the
/// format names a manifest, `<contract>.v<version>`, because that folder name
/// is a fact the format owns and nothing else in an output directory is: a
/// hand-written `src/` or a `node_modules/` beside the generated folders is
/// not this generator's to call stale.
fn committed_ts(out: &Path, layout: Layout) -> Result<BTreeSet<PathBuf>, String> {
    fn ts_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
        match std::fs::read_dir(dir) {
            Ok(listing) => Ok(listing
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "ts"))
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("cannot read `{}`: {e}", dir.display())),
        }
    }

    let mut committed: BTreeSet<PathBuf> = ts_files(out)?
        .into_iter()
        .filter_map(|p| p.file_name().map(PathBuf::from))
        .collect();
    if layout != Layout::Folder {
        return Ok(committed);
    }
    let listing = match std::fs::read_dir(out) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(committed),
        Err(e) => return Err(format!("cannot read `{}`: {e}", out.display())),
    };
    for entry in listing.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !path.is_dir() || !is_manifest_stem(&name) {
            continue;
        }
        for file in ts_files(&path)? {
            if let Some(file_name) = file.file_name() {
                committed.insert(PathBuf::from(&name).join(file_name));
            }
        }
    }
    Ok(committed)
}

/// Whether a directory name is `<contract>.v<version>` — the shape
/// `Directory::load` enforces on manifest file names, and therefore the shape
/// of every folder this generator writes.
fn is_manifest_stem(name: &str) -> bool {
    name.rsplit_once(".v").is_some_and(|(contract, version)| {
        !contract.is_empty() && !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Where two files first disagree, as one sentence.
///
/// A whole unified diff would be better and is what `diff -ru` gave the
/// precedent for free. This is the part of it that is actually acted on: the
/// first line that differs is where a reader looks, and a regeneration is what
/// they do next either way.
fn first_difference(have: &str, want: &str) -> String {
    let mut haves = have.lines();
    let mut wants = want.lines();
    let mut n = 0;
    loop {
        n += 1;
        match (haves.next(), wants.next()) {
            (None, None) => {
                return "contents match but the trailing newline does not".to_string();
            }
            (Some(a), Some(b)) if a == b => continue,
            (Some(a), Some(b)) => {
                return format!("line {n}\n      committed: {a}\n      generated: {b}");
            }
            (Some(a), None) => {
                return format!(
                    "line {n} is past the end of the generated file\n      committed: {a}"
                );
            }
            (None, Some(b)) => {
                return format!(
                    "the committed file ends at line {}\n      generated: {b}",
                    n - 1
                );
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn parse(args: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut positional: Vec<String> = Vec::new();
    let mut check = false;
    let mut opts = Options::default();
    let mut args = args.peekable();

    while let Some(arg) = args.next() {
        let mut value = |flag: &str| -> Result<String, String> {
            args.next()
                .ok_or_else(|| format!("`{flag}` needs a value; see --help"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--check" => check = true,
            "--barrel" => opts.barrel = true,
            "--ops" => opts.ops = true,
            "--layout" => {
                opts.layout = match value("--layout")?.as_str() {
                    "type" => Layout::Type,
                    "manifest" => Layout::Manifest,
                    "folder" => Layout::Folder,
                    other => {
                        return Err(format!(
                            "`--layout {other}`: expected `type`, `manifest` or `folder`"
                        ));
                    }
                }
            }
            "--only" => {
                let picked = value("--only")?;
                let selector = match picked.split_once('/') {
                    None => ContractSelector {
                        contract: picked,
                        version: None,
                    },
                    Some((contract, version)) => ContractSelector {
                        contract: contract.to_string(),
                        version: Some(version.parse().map_err(|_| {
                            format!(
                                "`--only {picked}`: expected `<contract>` or \
                                 `<contract>/<version>`, and `{version}` is not a version number"
                            )
                        })?),
                    },
                };
                opts.only.get_or_insert_with(Vec::new).push(selector);
            }
            "--wide-integers" => {
                opts.wide_integers = match value("--wide-integers")?.as_str() {
                    "bigint" => WideIntegers::Bigint,
                    "number" => WideIntegers::Number,
                    other => {
                        return Err(format!(
                            "`--wide-integers {other}`: expected `bigint` or `number`"
                        ));
                    }
                }
            }
            "--defaults" => {
                opts.defaults = match value("--defaults")?.as_str() {
                    "optional" => DefaultFields::Optional,
                    "required" => DefaultFields::Required,
                    other => {
                        return Err(format!(
                            "`--defaults {other}`: expected `optional` or `required`"
                        ));
                    }
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`; see --help"));
            }
            other => positional.push(other.to_string()),
        }
    }

    let [contracts, out] = positional.as_slice() else {
        return Err(format!(
            "expected a contracts directory and an output directory, got {}; see --help",
            positional.len()
        ));
    };
    Ok(Some(Args {
        contracts: PathBuf::from(contracts),
        out: PathBuf::from(out),
        check,
        opts,
    }))
}
