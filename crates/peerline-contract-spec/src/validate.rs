//! What a manifest must satisfy before anybody generates from it.
//!
//! Every check here is one a generator would otherwise have to make for
//! itself, and would make slightly differently — so a manifest that one
//! language accepted and another rejected would be a contract that exists in
//! one binding and not the other. [`super::Manifest::parse`] runs all of them
//! and there is no way to obtain an unvalidated manifest.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::{ErrorDef, Field, Manifest, OpDef, OpKind, Prose, Scalar, Schema, TypeDef};

/// A manifest that cannot be used: malformed JSON, or a rule below broken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn syntax(err: serde_json::Error) -> Self {
        Self::new(format!("not a valid contract manifest: {err}"))
    }

    /// The message, for a generator that has to report it in its own way — a
    /// compile error, a CLI diagnostic, a test failure.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl Manifest {
    /// Check every rule. Called by [`Manifest::parse`]; public so a tool that
    /// built a manifest in memory can check it too.
    pub fn validate(&self) -> Result<(), Error> {
        self.check_header()?;
        let types = self.check_types()?;
        self.check_errors(&types)?;
        self.check_ops(&types)?;
        check_no_direct_recursion(&types)
    }

    fn check_header(&self) -> Result<(), Error> {
        if self.contract.is_empty() {
            return Err(Error::new("`contract` must name the contract"));
        }
        if !self
            .contract
            .split('.')
            .all(|seg| is_lower_ident(seg) && !seg.is_empty())
        {
            return Err(Error::new(format!(
                "contract name `{}` must be dot-separated lower_snake_case segments",
                self.contract
            )));
        }
        if self.version == 0 {
            return Err(Error::new(
                "`version` is a major version and starts at 1; 0 is not a contract anyone can \
                 register",
            ));
        }
        check_prose("the contract", &self.description)
    }

    fn check_types(&self) -> Result<BTreeMap<&str, &TypeDef>, Error> {
        let mut types: BTreeMap<&str, &TypeDef> = BTreeMap::new();
        for def in &self.types {
            let name = def.name();
            if !is_type_ident(name) {
                return Err(Error::new(format!(
                    "type name `{name}` must be PascalCase and alphanumeric"
                )));
            }
            if types.insert(name, def).is_some() {
                return Err(Error::new(format!("type `{name}` is declared twice")));
            }
            check_prose(&format!("type `{name}`"), def.description())?;
        }
        // Resolution is a second pass: a type may legally reference one
        // declared after it, and requiring declaration order would make the
        // file's reading order a constraint rather than a choice.
        for def in &self.types {
            let name = def.name();
            match def {
                TypeDef::Struct { fields, .. } => check_fields(name, fields, &types)?,
                TypeDef::Enum { variants, .. } => {
                    if variants.is_empty() {
                        return Err(Error::new(format!("enum `{name}` has no variants")));
                    }
                    let mut seen = BTreeSet::new();
                    for v in variants {
                        if !is_lower_ident(&v.name) {
                            return Err(Error::new(format!(
                                "variant `{name}.{}` must be lower_snake_case",
                                v.name
                            )));
                        }
                        if !seen.insert(&v.name) {
                            return Err(Error::new(format!(
                                "variant `{name}.{}` is declared twice",
                                v.name
                            )));
                        }
                        check_prose(&format!("variant `{name}.{}`", v.name), &v.description)?;
                    }
                }
                TypeDef::Union { tag, variants, .. } => {
                    if !is_lower_ident(tag) {
                        return Err(Error::new(format!(
                            "union `{name}`'s tag `{tag}` must be lower_snake_case"
                        )));
                    }
                    if variants.is_empty() {
                        return Err(Error::new(format!("union `{name}` has no variants")));
                    }
                    let mut seen = BTreeSet::new();
                    for v in variants {
                        if !is_lower_ident(&v.name) {
                            return Err(Error::new(format!(
                                "variant `{name}.{}` must be lower_snake_case",
                                v.name
                            )));
                        }
                        if !seen.insert(&v.name) {
                            return Err(Error::new(format!(
                                "variant `{name}.{}` is declared twice",
                                v.name
                            )));
                        }
                        check_prose(&format!("variant `{name}.{}`", v.name), &v.description)?;
                        if v.fields.iter().any(|f| &f.name == tag) {
                            return Err(Error::new(format!(
                                "variant `{name}.{}` has a field named `{tag}`, which is the \
                                 union's own tag",
                                v.name
                            )));
                        }
                        check_fields(&format!("{name}.{}", v.name), &v.fields, &types)?;
                    }
                }
            }
        }
        Ok(types)
    }

    fn check_errors(&self, types: &BTreeMap<&str, &TypeDef>) -> Result<(), Error> {
        let mut names = BTreeSet::new();
        let mut codes = BTreeSet::new();
        for err in &self.errors {
            if !is_lower_ident(&err.name) {
                return Err(Error::new(format!(
                    "error name `{}` must be lower_snake_case",
                    err.name
                )));
            }
            if !names.insert(&err.name) {
                return Err(Error::new(format!(
                    "error `{}` is declared twice",
                    err.name
                )));
            }
            if !codes.insert(err.code) {
                return Err(Error::new(format!(
                    "error code {} is used by two errors of one contract; a consumer matches on \
                     the code and then decodes `data`, so two shapes behind one code is a \
                     mis-decode",
                    err.code
                )));
            }
            check_prose(&format!("error `{}`", err.name), &err.description)?;
            if let Some(data) = &err.data {
                check_schema(&format!("error `{}`'s data", err.name), data, types)?;
            }
        }
        Ok(())
    }

    fn check_ops(&self, types: &BTreeMap<&str, &TypeDef>) -> Result<(), Error> {
        if self.ops.is_empty() {
            return Err(Error::new(
                "a contract with no ops is a name, not a contract",
            ));
        }
        let errors: BTreeSet<&str> = self.errors.iter().map(|e| e.name.as_str()).collect();
        let mut names = BTreeSet::new();
        let mut consts = BTreeMap::new();
        for op in &self.ops {
            let prefix = format!("{}.", self.contract);
            if !op.name.starts_with(&prefix) {
                return Err(Error::new(format!(
                    "op `{}` is not namespaced under `{}`; two contracts on one peer would \
                     collide",
                    op.name, self.contract
                )));
            }
            if !op
                .name
                .split('.')
                .all(|seg| !seg.is_empty() && is_lower_ident(seg))
            {
                return Err(Error::new(format!(
                    "op name `{}` must be dot-separated lower_snake_case segments",
                    op.name
                )));
            }
            if !names.insert(&op.name) {
                return Err(Error::new(format!("op `{}` is declared twice", op.name)));
            }
            let short = op.short_name(&self.contract);
            let konst = crate::const_name("OP", short);
            if let Some(other) = consts.insert(konst.clone(), op.name.clone()) {
                return Err(Error::new(format!(
                    "ops `{other}` and `{}` both give the constant `{konst}`",
                    op.name
                )));
            }
            check_prose(&format!("op `{}`", op.name), &op.description)?;
            check_op_shape(op, types)?;
            for name in &op.errors {
                if !errors.contains(name.as_str()) {
                    return Err(Error::new(format!(
                        "op `{}` raises `{name}`, which is not in this contract's `errors`",
                        op.name
                    )));
                }
            }
        }
        Ok(())
    }
}

/// The rule `kind` exists to enforce.
///
/// A unary op answers once and a stream op emits items, and the two are
/// different peerline calls. A manifest that declared a stream and then
/// described a `result` would be read one way by a producer and the other way
/// by a consumer, which is exactly the mismatch that only shows up at runtime.
fn check_op_shape(op: &OpDef, types: &BTreeMap<&str, &TypeDef>) -> Result<(), Error> {
    match op.kind {
        OpKind::Unary => {
            if op.item.is_some() {
                return Err(Error::new(format!(
                    "op `{}` is unary but declares `item`; only a stream emits items",
                    op.name
                )));
            }
            if let Some(result) = &op.result {
                check_schema(&format!("op `{}`'s result", op.name), result, types)?;
            }
        }
        OpKind::Stream => {
            if op.result.is_some() {
                return Err(Error::new(format!(
                    "op `{}` is a stream but declares `result`; a stream answers with items and \
                     a terminal, not a value",
                    op.name
                )));
            }
            let Some(item) = &op.item else {
                return Err(Error::new(format!(
                    "op `{}` is a stream and must declare the `item` it emits",
                    op.name
                )));
            };
            check_schema(&format!("op `{}`'s item", op.name), item, types)?;
        }
    }
    // peerline rejects params that are not a JSON object, so an op whose args
    // are a scalar or a list is one nobody can call. Caught here rather than
    // in each generator's test suite.
    if let Some(args) = &op.args {
        check_schema(&format!("op `{}`'s args", op.name), args, types)?;
        let Schema::Named(name) = args else {
            return Err(Error::new(format!(
                "op `{}`'s args must be a struct: peerline rejects params that are not an object",
                op.name
            )));
        };
        match types.get(name.as_str()) {
            Some(TypeDef::Struct { .. }) => {}
            _ => {
                return Err(Error::new(format!(
                    "op `{}`'s args must be a struct: peerline rejects params that are not an \
                     object",
                    op.name
                )));
            }
        }
    }
    Ok(())
}

fn check_fields(
    owner: &str,
    fields: &[Field],
    types: &BTreeMap<&str, &TypeDef>,
) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for field in fields {
        if !is_lower_ident(&field.name) {
            return Err(Error::new(format!(
                "field `{owner}.{}` must be lower_snake_case",
                field.name
            )));
        }
        if !seen.insert(&field.name) {
            return Err(Error::new(format!(
                "field `{owner}.{}` is declared twice",
                field.name
            )));
        }
        check_prose(
            &format!("field `{owner}.{}`", field.name),
            &field.description,
        )?;
        check_schema(&format!("field `{owner}.{}`", field.name), &field.ty, types)?;
    }
    Ok(())
}

fn check_schema(
    owner: &str,
    schema: &Schema,
    types: &BTreeMap<&str, &TypeDef>,
) -> Result<(), Error> {
    match schema {
        Schema::Named(name) => {
            if Scalar::from_keyword(name).is_some() || types.contains_key(name.as_str()) {
                Ok(())
            } else {
                Err(Error::new(format!(
                    "{owner} names `{name}`, which is neither a scalar keyword nor a type in \
                     this manifest"
                )))
            }
        }
        Schema::Array { array } => check_schema(owner, array, types),
    }
}

/// A type that contains itself by value has no size, in Rust and in most
/// targets that generate records. A list breaks the cycle, so the error says
/// so rather than leaving the author to work out why the generated code will
/// not compile.
fn check_no_direct_recursion<'a>(types: &BTreeMap<&'a str, &'a TypeDef>) -> Result<(), Error> {
    fn direct_refs(def: &TypeDef) -> Vec<&str> {
        fn push<'a>(out: &mut Vec<&'a str>, fields: &'a [Field]) {
            for field in fields {
                if let Schema::Named(name) = &field.ty {
                    out.push(name.as_str());
                }
            }
        }
        let mut out = Vec::new();
        match def {
            TypeDef::Struct { fields, .. } => push(&mut out, fields),
            // A closed set of strings holds nothing, so it ends every walk.
            TypeDef::Enum { .. } => {}
            TypeDef::Union { variants, .. } => {
                for variant in variants {
                    push(&mut out, &variant.fields);
                }
            }
        }
        out
    }

    fn walk<'a>(
        name: &'a str,
        types: &BTreeMap<&'a str, &'a TypeDef>,
        path: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Result<(), Error> {
        if done.contains(name) {
            return Ok(());
        }
        if let Some(at) = path.iter().position(|n| *n == name) {
            let mut cycle: Vec<&str> = path[at..].to_vec();
            cycle.push(name);
            return Err(Error::new(format!(
                "types {} contain one another by value and have no size; put a list in the cycle",
                cycle.join(" -> ")
            )));
        }
        let Some(def) = types.get(name).copied() else {
            return Ok(());
        };
        path.push(name);
        for next in direct_refs(def) {
            walk(next, types, path, done)?;
        }
        path.pop();
        done.insert(name);
        Ok(())
    }

    let mut done = BTreeSet::new();
    for name in types.keys().copied() {
        walk(name, types, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

/// Prose must exist and must be plain.
///
/// The link check is not pedantry. The manifest is the one copy of the
/// reasoning, read by an implementor in any language, and rustdoc's
/// `[`Type`]` shorthand renders as literal brackets and backticks everywhere
/// except rustdoc. Letting it in would make the shared file quietly
/// Rust-shaped, which is the bias this whole format exists to remove.
fn check_prose(owner: &str, prose: &Prose) -> Result<(), Error> {
    if prose.is_empty() {
        return Err(Error::new(format!(
            "{owner} has no description; the reasoning for a shape lives in the manifest, so an \
             undescribed one is a manifest that is not finished"
        )));
    }
    for line in prose.lines() {
        if line.contains("[`") {
            return Err(Error::new(format!(
                "{owner}'s description uses rustdoc link syntax (`[`…`]`); a manifest carries \
                 plain prose, because its readers are not necessarily writing Rust"
            )));
        }
        if line.contains("](") {
            return Err(Error::new(format!(
                "{owner}'s description uses markdown link syntax (`[…](…)`); a manifest carries \
                 plain prose"
            )));
        }
    }
    Ok(())
}

fn is_lower_ident(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_type_ident(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

impl ErrorDef {
    /// The constant a Rust generator emits for this error: `ERR_` plus the
    /// screaming form of the name.
    pub fn const_name(&self) -> String {
        crate::const_name("ERR", &self.name)
    }
}
