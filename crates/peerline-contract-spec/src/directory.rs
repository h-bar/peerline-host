//! A contracts directory, loaded whole.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Error, Manifest, TypeDef};

/// Every manifest in one contracts directory, checked against each other.
///
/// [`Manifest::parse`] checks one file, and three things about a contract
/// are not checkable from one file: an import naming a type the other
/// manifest does not declare, two contracts picking the same error code, and
/// a file whose name disagrees with what it declares. All three need the
/// whole directory in hand, which is what this holds.
///
/// The directory — not the file — is the unit a contract is written in. It
/// is what a non-Rust implementor clones, it is what `contract!` loads, and
/// so all three checks are a compile error at every crate that expands any
/// manifest in it, rather than a test somebody remembered to write.
#[derive(Clone, Debug)]
pub struct Directory {
    manifests: Vec<Manifest>,
    files: Vec<PathBuf>,
}

impl Directory {
    /// Read every `*.json` file in `dir`, parse each, and check them against
    /// each other.
    ///
    /// A file whose name is not `<contract>.v<version>.json` is rejected:
    /// an import names a contract and a version, never a path, so the
    /// convention is the only way an implementor who is not reading Rust
    /// finds the file an import refers to.
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let listing = std::fs::read_dir(dir).map_err(|err| {
            Error::new(format!(
                "cannot read the contracts directory `{}`: {err}",
                dir.display()
            ))
        })?;
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in listing {
            let path = entry
                .map_err(|err| Error::new(format!("reading `{}`: {err}", dir.display())))?
                .path();
            if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
        // Sorted so that an error names the same file on every machine.
        files.sort();
        if files.is_empty() {
            return Err(Error::new(format!(
                "`{}` holds no manifests; a contracts directory is the unit a contract is \
                 written in, so an empty one is a path that is wrong rather than a contract \
                 that says nothing",
                dir.display()
            )));
        }
        let mut manifests = Vec::with_capacity(files.len());
        for path in &files {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let text = std::fs::read_to_string(path)
                .map_err(|err| Error::new(format!("cannot read `{name}`: {err}")))?;
            let manifest =
                Manifest::parse(&text).map_err(|err| Error::new(format!("`{name}`: {err}")))?;
            let expected = format!("{}.v{}.json", manifest.contract, manifest.version);
            if name != expected {
                return Err(Error::new(format!(
                    "`{name}` declares `{}`, so it must be named `{expected}`; an import names \
                     a contract and a version and never a path, and the file name is how an \
                     implementor finds what it refers to",
                    manifest.id()
                )));
            }
            manifests.push(manifest);
        }
        let mut loaded = Self::new(manifests)?;
        loaded.files = files;
        Ok(loaded)
    }

    /// The same checks over manifests already in hand, for a caller that
    /// built them rather than read them.
    pub fn new(manifests: Vec<Manifest>) -> Result<Self, Error> {
        let mut ids: BTreeMap<String, ()> = BTreeMap::new();
        for manifest in &manifests {
            if ids.insert(manifest.id(), ()).is_some() {
                return Err(Error::new(format!(
                    "`{}` is declared twice in one directory",
                    manifest.id()
                )));
            }
        }
        for manifest in &manifests {
            manifest.check_imports(&manifests)?;
        }
        // One error-code space, because a consumer matches on the code and
        // then decodes the payload behind it: a collision between two
        // contracts mounted in one process is a mis-decode rather than a
        // confusing message. Obvious within a file, invisible across a
        // directory, so it is checked here.
        let mut codes: BTreeMap<i32, String> = BTreeMap::new();
        for manifest in &manifests {
            for err in &manifest.errors {
                if let Some(other) = codes.insert(err.code, manifest.id()) {
                    return Err(Error::new(format!(
                        "error code {} is declared by `{}` and by `{other}`",
                        err.code,
                        manifest.id()
                    )));
                }
            }
        }
        Ok(Self {
            manifests,
            files: Vec::new(),
        })
    }

    /// The manifest declaring a contract at a version, as an import or a
    /// call site names it.
    pub fn get(&self, contract: &str, version: u32) -> Option<&Manifest> {
        self.manifests
            .iter()
            .find(|m| m.contract == contract && m.version == version)
    }

    /// Every manifest, in file-name order.
    pub fn manifests(&self) -> &[Manifest] {
        &self.manifests
    }

    /// The files read, in the same order. Empty for a directory built by
    /// [`Directory::new`].
    ///
    /// A generator that caches its output needs these: each one is an input,
    /// and a build system that is not told so will go on using an expansion
    /// of a manifest that has since changed.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// What a type name means in `from`'s scope: its own declaration, or the
    /// one in the manifest `from` imports it from.
    ///
    /// This is the whole of what the directory buys a generator over a file.
    /// An imported shape's fields are in another file, and with the
    /// directory loaded they are readable — so anything a generator derives
    /// from a shape it derives the same way whether the shape is local or
    /// imported.
    pub fn resolve<'a>(
        &'a self,
        from: &'a Manifest,
        name: &str,
    ) -> Option<(&'a Manifest, &'a TypeDef)> {
        if let Some(def) = from.types.iter().find(|def| def.name() == name) {
            return Some((from, def));
        }
        let import = from
            .imports
            .iter()
            .find(|import| import.types.iter().any(|ty| ty == name))?;
        let source = self.get(&import.contract, import.version)?;
        let def = source.types.iter().find(|def| def.name() == name)?;
        Some((source, def))
    }
}
