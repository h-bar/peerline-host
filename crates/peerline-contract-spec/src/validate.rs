//! What a manifest must satisfy before anybody generates from it.
//!
//! Every check here is one a generator would otherwise have to make for
//! itself, and would make slightly differently — so a manifest that one
//! language accepted and another rejected would be a contract that exists in
//! one binding and not the other. [`super::Manifest::parse`] runs all of them
//! and there is no way to obtain an unvalidated manifest.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;

use crate::{
    EnumVariant, ErrorDef, Field, Import, Manifest, OpDef, OpKind, Prose, Role, Scalar, Schema,
    Tagging, TypeDef, UnionVariant,
};

/// A manifest that cannot be used: malformed JSON, or a rule below broken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
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

/// Every name a schema in this manifest may resolve to.
///
/// Two halves, because they are checkable to different depths. A local type
/// is here in full, so its fields, its recursion and its defaults are all
/// checked. An imported one is a name and the import that brought it: this
/// crate parses one file and cannot open another, so an imported shape is
/// taken on trust *within* a file and checked across files by
/// [`Manifest::check_imports`], where both are in hand.
pub(crate) struct Scope<'a> {
    pub(crate) types: BTreeMap<&'a str, &'a TypeDef>,
    pub(crate) imported: BTreeMap<&'a str, &'a Import>,
}

impl<'a> Scope<'a> {
    fn resolves(&self, name: &str) -> bool {
        self.types.contains_key(name) || self.imported.contains_key(name)
    }

    fn local(&self, name: &str) -> Option<&'a TypeDef> {
        self.types.get(name).copied()
    }

    fn is_imported(&self, name: &str) -> bool {
        self.imported.contains_key(name)
    }
}

impl Manifest {
    /// Check every rule. Called by [`Manifest::parse`]; public so a tool that
    /// built a manifest in memory can check it too.
    pub fn validate(&self) -> Result<(), Error> {
        self.check_header()?;
        let scope = self.check_imports_and_types()?;
        self.check_errors(&scope)?;
        self.check_ops(&scope)?;
        check_no_direct_recursion(&scope)
    }

    /// Check each [`Manifest::imports`] against the manifest it names.
    ///
    /// Separate from [`Manifest::validate`] because it needs a file that one
    /// is not: a manifest names a contract and a version, never a path, so
    /// the other file is only in hand once a whole directory is.
    /// [`Directory::new`](crate::Directory::new) is what has it, and calls
    /// this for every manifest it holds.
    pub fn check_imports(&self, available: &[Manifest]) -> Result<(), Error> {
        for import in &self.imports {
            let Some(source) = available
                .iter()
                .find(|m| m.contract == import.contract && m.version == import.version)
            else {
                return Err(Error::new(format!(
                    "`{}` imports from `{}`, which is not among the manifests given",
                    self.contract,
                    import.id()
                )));
            };
            let declared: BTreeSet<&str> = source.types.iter().map(TypeDef::name).collect();
            for name in &import.types {
                if !declared.contains(name.as_str()) {
                    return Err(Error::new(format!(
                        "`{}` imports `{name}` from `{}`, which declares no such type",
                        self.contract,
                        import.id()
                    )));
                }
            }
        }
        Ok(())
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
        match self.role {
            Role::Contract => {
                if self.ops.is_empty() {
                    return Err(Error::new(
                        "a contract with no ops is a name, not a contract; a file that exists to \
                         be imported declares `\"role\": \"vocabulary\"`",
                    ));
                }
            }
            Role::Vocabulary => {
                if !self.ops.is_empty() {
                    return Err(Error::new(format!(
                        "`{}` is a vocabulary and declares ops; nobody produces a vocabulary, so \
                         nobody could answer them",
                        self.contract
                    )));
                }
                if !self.errors.is_empty() {
                    return Err(Error::new(format!(
                        "`{}` is a vocabulary and declares errors; an error code is something a \
                         producer raises, and a vocabulary has no producer",
                        self.contract
                    )));
                }
                if self.types.is_empty() {
                    return Err(Error::new(format!(
                        "`{}` is a vocabulary and declares no types, so it says nothing",
                        self.contract
                    )));
                }
            }
        }
        check_prose("the contract", &self.description)
    }

    fn check_imports_and_types(&self) -> Result<Scope<'_>, Error> {
        let mut imported: BTreeMap<&str, &Import> = BTreeMap::new();
        let mut sources: BTreeSet<&str> = BTreeSet::new();
        for import in &self.imports {
            if !import
                .contract
                .split('.')
                .all(|seg| is_lower_ident(seg) && !seg.is_empty())
            {
                return Err(Error::new(format!(
                    "imported contract name `{}` must be dot-separated lower_snake_case segments",
                    import.contract
                )));
            }
            if import.version == 0 {
                return Err(Error::new(format!(
                    "import of `{}` names version 0; a major version starts at 1",
                    import.contract
                )));
            }
            if import.contract == self.contract {
                return Err(Error::new(format!(
                    "`{}` imports from itself; a type declared here is already in scope",
                    self.contract
                )));
            }
            if !sources.insert(import.contract.as_str()) {
                return Err(Error::new(format!(
                    "`{}` is imported twice; type names share one namespace, so two versions of \
                     one vocabulary in one manifest would be two shapes with one name",
                    import.contract
                )));
            }
            if import.types.is_empty() {
                return Err(Error::new(format!(
                    "the import of `{}` names no types, so it brings nothing into scope",
                    import.id()
                )));
            }
            check_prose(
                &format!("the import of `{}`", import.id()),
                &import.description,
            )?;
            for name in &import.types {
                if !is_type_ident(name) {
                    return Err(Error::new(format!(
                        "imported type name `{name}` must be PascalCase and alphanumeric"
                    )));
                }
                if imported.insert(name.as_str(), import).is_some() {
                    return Err(Error::new(format!("type `{name}` is imported twice")));
                }
            }
        }

        let mut types: BTreeMap<&str, &TypeDef> = BTreeMap::new();
        for def in &self.types {
            let name = def.name();
            if !is_type_ident(name) {
                return Err(Error::new(format!(
                    "type name `{name}` must be PascalCase and alphanumeric"
                )));
            }
            if imported.contains_key(name) {
                return Err(Error::new(format!(
                    "type `{name}` is declared here and also imported; one name cannot be two \
                     shapes"
                )));
            }
            if types.insert(name, def).is_some() {
                return Err(Error::new(format!("type `{name}` is declared twice")));
            }
            check_prose(&format!("type `{name}`"), def.description())?;
        }

        let scope = Scope { types, imported };
        // Resolution is a second pass: a type may legally reference one
        // declared after it, and requiring declaration order would make the
        // file's reading order a constraint rather than a choice.
        for def in &self.types {
            let name = def.name();
            match def {
                TypeDef::Struct { fields, .. } => check_fields(name, fields, None, &scope)?,
                TypeDef::Enum { variants, .. } => check_enum(name, variants)?,
                TypeDef::Union {
                    tagging,
                    tag,
                    variants,
                    ..
                } => check_union(name, *tagging, tag.as_deref(), variants, &scope)?,
            }
        }
        Ok(scope)
    }

    fn check_errors(&self, scope: &Scope<'_>) -> Result<(), Error> {
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
                check_schema(
                    &format!("error `{}`'s data", err.name),
                    data,
                    scope,
                    Nullability::Allowed,
                )?;
            }
        }
        Ok(())
    }

    fn check_ops(&self, scope: &Scope<'_>) -> Result<(), Error> {
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
            check_op_shape(op, scope)?;
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

/// Where a `{ "nullable": … }` schema is a legal thing to write.
///
/// Only where there is no field to carry `nullable`. A field has to
/// distinguish absent from null and a schema cannot, so allowing both
/// spellings in a field position would be two ways to say one thing — and
/// two ways that a reader would reasonably expect to mean different things.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nullability {
    Allowed,
    Forbidden,
}

/// The rule `kind` exists to enforce.
///
/// A unary op answers once and a stream op emits items, and the two are
/// different peerline calls. A manifest that declared a stream and then
/// described a `result` would be read one way by a producer and the other way
/// by a consumer, which is exactly the mismatch that only shows up at runtime.
fn check_op_shape(op: &OpDef, scope: &Scope<'_>) -> Result<(), Error> {
    match op.kind {
        OpKind::Unary => {
            if op.item.is_some() {
                return Err(Error::new(format!(
                    "op `{}` is unary but declares `item`; only a stream emits items",
                    op.name
                )));
            }
            if let Some(result) = &op.result {
                check_schema(
                    &format!("op `{}`'s result", op.name),
                    result,
                    scope,
                    Nullability::Allowed,
                )?;
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
            check_schema(
                &format!("op `{}`'s item", op.name),
                item,
                scope,
                Nullability::Allowed,
            )?;
        }
    }
    // peerline rejects params that are not a JSON object, so an op whose args
    // are a scalar or a list is one nobody can call. Caught here rather than
    // in each generator's test suite.
    if let Some(args) = &op.args {
        check_schema(
            &format!("op `{}`'s args", op.name),
            args,
            scope,
            Nullability::Forbidden,
        )?;
        let Schema::Named(name) = args else {
            return Err(Error::new(format!(
                "op `{}`'s args must be a struct: peerline rejects params that are not an object",
                op.name
            )));
        };
        let ok = match scope.local(name) {
            Some(TypeDef::Struct { .. }) => true,
            // An imported args shape is taken on trust here for the same
            // reason every imported shape is: this file does not hold it.
            None => scope.is_imported(name),
            Some(_) => false,
        };
        if !ok {
            return Err(Error::new(format!(
                "op `{}`'s args must be a struct: peerline rejects params that are not an object",
                op.name
            )));
        }
    }
    Ok(())
}

fn check_enum(name: &str, variants: &[EnumVariant]) -> Result<(), Error> {
    if variants.is_empty() {
        return Err(Error::new(format!("enum `{name}` has no variants")));
    }
    let mut seen = BTreeSet::new();
    let mut wires = BTreeSet::new();
    for v in variants {
        if !is_lower_ident(&v.name) {
            return Err(Error::new(format!(
                "variant `{name}.{}` must be lower_snake_case; the literal wire value goes in \
                 `wire`, which may be any string",
                v.name
            )));
        }
        if !seen.insert(&v.name) {
            return Err(Error::new(format!(
                "variant `{name}.{}` is declared twice",
                v.name
            )));
        }
        check_wire(&format!("variant `{name}.{}`", v.name), v.wire.as_deref())?;
        if !wires.insert(v.wire()) {
            return Err(Error::new(format!(
                "two variants of `{name}` both travel as `{}`; a decoder could not tell them \
                 apart",
                v.wire()
            )));
        }
        check_prose(&format!("variant `{name}.{}`", v.name), &v.description)?;
    }
    Ok(())
}

fn check_union(
    name: &str,
    tagging: Tagging,
    tag: Option<&str>,
    variants: &[UnionVariant],
    scope: &Scope<'_>,
) -> Result<(), Error> {
    let tag = match (tagging, tag) {
        (Tagging::Internal, Some(tag)) => {
            if !is_lower_ident(tag) {
                return Err(Error::new(format!(
                    "union `{name}`'s tag `{tag}` must be lower_snake_case"
                )));
            }
            Some(tag)
        }
        (Tagging::Internal, None) => {
            return Err(Error::new(format!(
                "union `{name}` is internally tagged and must name the `tag` field carrying the \
                 discriminator"
            )));
        }
        (Tagging::External, None) => None,
        (Tagging::External, Some(tag)) => {
            return Err(Error::new(format!(
                "union `{name}` is externally tagged and also declares the tag `{tag}`; an \
                 externally tagged arm has no discriminator field — the arm's name is the key"
            )));
        }
    };
    if variants.is_empty() {
        return Err(Error::new(format!("union `{name}` has no variants")));
    }
    let mut seen = BTreeSet::new();
    let mut wires = BTreeSet::new();
    for v in variants {
        let owner = format!("{name}.{}", v.name);
        if !is_lower_ident(&v.name) {
            return Err(Error::new(format!(
                "variant `{owner}` must be lower_snake_case; the literal wire value goes in \
                 `wire`, which may be any string"
            )));
        }
        if !seen.insert(&v.name) {
            return Err(Error::new(format!("variant `{owner}` is declared twice")));
        }
        check_wire(&format!("variant `{owner}`"), v.wire.as_deref())?;
        if !wires.insert(v.wire()) {
            return Err(Error::new(format!(
                "two variants of `{name}` both travel as `{}`; a decoder could not tell them \
                 apart",
                v.wire()
            )));
        }
        check_prose(&format!("variant `{owner}`"), &v.description)?;
        match (&v.payload, v.fields.is_empty()) {
            (Some(_), false) => {
                return Err(Error::new(format!(
                    "variant `{owner}` declares both `fields` and a `payload`; it carries one \
                     shape or a set of keys, not both"
                )));
            }
            (Some(payload), true) => {
                check_schema(
                    &format!("variant `{owner}`'s payload"),
                    payload,
                    scope,
                    Nullability::Forbidden,
                )?;
                if tagging == Tagging::Internal {
                    check_internal_payload(&owner, payload, tag, scope)?;
                }
            }
            (None, _) => {
                if let Some(tag) = tag
                    && v.fields.iter().any(|f| f.name == tag)
                {
                    return Err(Error::new(format!(
                        "variant `{owner}` has a field named `{tag}`, which is the union's own \
                         tag"
                    )));
                }
                check_fields(&owner, &v.fields, tag, scope)?;
            }
        }
    }
    Ok(())
}

/// An internally tagged arm puts the discriminator *inside* the payload's
/// object, so the payload has to be one — and it has to have room for the
/// key.
fn check_internal_payload(
    owner: &str,
    payload: &Schema,
    tag: Option<&str>,
    scope: &Scope<'_>,
) -> Result<(), Error> {
    let Schema::Named(name) = payload else {
        return Err(Error::new(format!(
            "variant `{owner}`'s payload is a list, and an internally tagged arm has nowhere to \
             put the discriminator inside one; declare the union `\"tagging\": \"external\"`"
        )));
    };
    if Scalar::from_keyword(name).is_some() {
        return Err(Error::new(format!(
            "variant `{owner}`'s payload is the scalar `{name}`, and an internally tagged arm \
             has nowhere to put the discriminator inside one; declare the union \
             `\"tagging\": \"external\"`"
        )));
    }
    match scope.local(name) {
        Some(TypeDef::Struct { fields, .. }) => {
            if let Some(tag) = tag
                && fields.iter().any(|f| f.name == tag)
            {
                return Err(Error::new(format!(
                    "variant `{owner}`'s payload `{name}` has a field named `{tag}`, which is \
                     the union's own tag; the two would be one key on the wire"
                )));
            }
            Ok(())
        }
        Some(TypeDef::Union { .. }) | None => Ok(()),
        Some(TypeDef::Enum { .. }) => Err(Error::new(format!(
            "variant `{owner}`'s payload is the enum `{name}`, which travels as a string; an \
             internally tagged arm has nowhere to put the discriminator inside one"
        ))),
    }
}

fn check_wire(owner: &str, wire: Option<&str>) -> Result<(), Error> {
    match wire {
        Some(text) if text.trim().is_empty() => Err(Error::new(format!(
            "{owner}'s `wire` value is blank; a wire value is the literal text a decoder matches"
        ))),
        _ => Ok(()),
    }
}

fn check_fields(
    owner: &str,
    fields: &[Field],
    tag: Option<&str>,
    scope: &Scope<'_>,
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
        let here = format!("field `{owner}.{}`", field.name);
        check_prose(&here, &field.description)?;
        check_schema(&here, &field.ty, scope, Nullability::Forbidden)?;
        check_field_state(&here, field, scope)?;
        if let Some(default) = &field.default {
            check_value(&format!("{here}'s default"), default, &field.ty, scope)?;
        }
    }
    check_key_collisions(owner, fields, tag, scope)
}

/// The three states a field may be in, and the combinations that are not a
/// state at all.
fn check_field_state(here: &str, field: &Field, scope: &Scope<'_>) -> Result<(), Error> {
    if field.optional && field.nullable {
        return Err(Error::new(format!(
            "{here} is both `optional` and `nullable`; absent and null are the two answers, and \
             a field gives one of them — which one is what a decoder in another language needs \
             to know"
        )));
    }
    if field.default.is_some() && (field.optional || field.nullable) {
        return Err(Error::new(format!(
            "{here} has a `default` and is also optional or nullable; a default is what an \
             absent key means, so the field is neither absent nor null once it is read"
        )));
    }
    if field.flatten {
        if field.optional || field.nullable || field.default.is_some() {
            return Err(Error::new(format!(
                "{here} is flattened and also optional, nullable or defaulted; a flattened field \
                 has no key of its own, so there is nothing for any of those to be about"
            )));
        }
        let Schema::Named(name) = &field.ty else {
            return Err(Error::new(format!(
                "{here} is flattened but is a list; only an object's keys can be written beside \
                 its siblings"
            )));
        };
        if Scalar::from_keyword(name).is_some() {
            return Err(Error::new(format!(
                "{here} is flattened but is the scalar `{name}`; only an object's keys can be \
                 written beside its siblings"
            )));
        }
        match scope.local(name) {
            Some(TypeDef::Struct { .. }) | None => {}
            Some(_) => {
                return Err(Error::new(format!(
                    "{here} is flattened but `{name}` is not a struct; only an object's keys can \
                     be written beside its siblings"
                )));
            }
        }
    }
    Ok(())
}

/// A flattened field contributes its type's keys to the enclosing object, so
/// the enclosing object is where a collision between them happens — and a
/// collision is two fields serialising as one key.
fn check_key_collisions(
    owner: &str,
    fields: &[Field],
    tag: Option<&str>,
    scope: &Scope<'_>,
) -> Result<(), Error> {
    let mut keys: BTreeSet<String> = BTreeSet::new();
    if let Some(tag) = tag {
        keys.insert(tag.to_string());
    }
    for field in fields {
        if !field.flatten {
            if !keys.insert(field.name.clone()) {
                return Err(Error::new(format!(
                    "`{owner}` writes the key `{}` twice, once through a flattened field",
                    field.name
                )));
            }
            continue;
        }
        let Schema::Named(name) = &field.ty else {
            continue;
        };
        // An imported shape's keys are not in this file; the cross-file check
        // and the target's own compiler are what catch a collision with one.
        for key in flattened_keys(name, scope, &mut BTreeSet::new()) {
            if !keys.insert(key.clone()) {
                return Err(Error::new(format!(
                    "`{owner}` writes the key `{key}` twice, once through the flattened field \
                     `{}`",
                    field.name
                )));
            }
        }
    }
    Ok(())
}

fn flattened_keys(name: &str, scope: &Scope<'_>, seen: &mut BTreeSet<String>) -> Vec<String> {
    if !seen.insert(name.to_string()) {
        return Vec::new();
    }
    let Some(TypeDef::Struct { fields, .. }) = scope.local(name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for field in fields {
        if field.flatten {
            if let Schema::Named(inner) = &field.ty {
                out.extend(flattened_keys(inner, scope, seen));
            }
        } else {
            out.push(field.name.clone());
        }
    }
    out
}

fn check_schema(
    owner: &str,
    schema: &Schema,
    scope: &Scope<'_>,
    nullability: Nullability,
) -> Result<(), Error> {
    match schema {
        Schema::Named(name) => {
            if Scalar::from_keyword(name).is_some() || scope.resolves(name) {
                Ok(())
            } else {
                Err(Error::new(format!(
                    "{owner} names `{name}`, which is neither a scalar keyword, nor a type in \
                     this manifest, nor an imported one"
                )))
            }
        }
        Schema::Array { array } => check_schema(owner, array, scope, Nullability::Forbidden),
        Schema::Nullable { nullable } => {
            if nullability == Nullability::Forbidden {
                return Err(Error::new(format!(
                    "{owner} is written as a nullable schema, which is only how a value with no \
                     field of its own says it may be null; a field says `\"nullable\": true`, \
                     because a field must also distinguish absent from null"
                )));
            }
            check_schema(owner, nullable, scope, Nullability::Forbidden)
        }
    }
}

/// A default is JSON, and it has to be JSON of the type the field declared —
/// otherwise the manifest states a default that one generator emits and
/// another rejects.
fn check_value(
    owner: &str,
    value: &Value,
    schema: &Schema,
    scope: &Scope<'_>,
) -> Result<(), Error> {
    let mismatch = |expected: &str| {
        Err(Error::new(format!(
            "{owner} is `{value}`, which is not {expected}"
        )))
    };
    match schema {
        Schema::Nullable { nullable } => {
            if value.is_null() {
                Ok(())
            } else {
                check_value(owner, value, nullable, scope)
            }
        }
        Schema::Array { array } => {
            let Some(items) = value.as_array() else {
                return mismatch("a list");
            };
            for item in items {
                check_value(owner, item, array, scope)?;
            }
            Ok(())
        }
        Schema::Named(name) => {
            if let Some(scalar) = Scalar::from_keyword(name) {
                return check_scalar_value(owner, value, scalar);
            }
            if scope.is_imported(name) {
                return Err(Error::new(format!(
                    "{owner} names the imported type `{name}`, whose shape is in another \
                     manifest and cannot be checked from here; a default has to be checkable \
                     where it is written"
                )));
            }
            match scope.local(name) {
                Some(TypeDef::Enum { variants, .. }) => {
                    let Some(text) = value.as_str() else {
                        return mismatch("one of the enum's values");
                    };
                    if variants.iter().any(|v| v.wire() == text) {
                        Ok(())
                    } else {
                        mismatch(&format!("one of `{name}`'s values"))
                    }
                }
                Some(TypeDef::Struct { fields, .. }) => {
                    check_object_value(owner, value, name, fields, scope)
                }
                Some(TypeDef::Union { .. }) => Err(Error::new(format!(
                    "{owner} defaults a union; which arm a value takes when nobody said is a \
                     choice a contract should make in prose and an op should take explicitly"
                ))),
                None => Ok(()),
            }
        }
    }
}

fn check_object_value(
    owner: &str,
    value: &Value,
    name: &str,
    fields: &[Field],
    scope: &Scope<'_>,
) -> Result<(), Error> {
    let Some(object) = value.as_object() else {
        return Err(Error::new(format!(
            "{owner} is `{value}`, which is not an object, and `{name}` is a struct"
        )));
    };
    if fields.iter().any(|f| f.flatten) {
        return Err(Error::new(format!(
            "{owner} defaults `{name}`, which flattens another shape; the keys of a flattened \
             field are not all in this file, so the default cannot be checked here"
        )));
    }
    let declared: BTreeMap<&str, &Field> = fields.iter().map(|f| (f.name.as_str(), f)).collect();
    for key in object.keys() {
        if !declared.contains_key(key.as_str()) {
            return Err(Error::new(format!(
                "{owner} sets `{key}`, which is not a field of `{name}`"
            )));
        }
    }
    for field in fields {
        match object.get(&field.name) {
            Some(inner) => check_value(
                &format!("{owner}'s `{}`", field.name),
                inner,
                &field.ty,
                scope,
            )?,
            None => {
                if field.is_required() {
                    return Err(Error::new(format!(
                        "{owner} omits `{}`, which `{name}` requires",
                        field.name
                    )));
                }
            }
        }
    }
    Ok(())
}

fn check_scalar_value(owner: &str, value: &Value, scalar: Scalar) -> Result<(), Error> {
    let ok = match scalar {
        Scalar::Json => true,
        Scalar::String | Scalar::Path => value.is_string(),
        Scalar::Bool => value.is_boolean(),
        Scalar::F64 => value.is_f64() || value.is_i64() || value.is_u64(),
        Scalar::I32 => value.as_i64().is_some_and(|n| i32::try_from(n).is_ok()),
        Scalar::U32 => value.as_u64().is_some_and(|n| u32::try_from(n).is_ok()),
        // The bound is the contract's own claim about the value, so a
        // default outside it is the manifest contradicting itself in the
        // one place the format can catch it.
        Scalar::I53 => value
            .as_i64()
            .is_some_and(|n| n.unsigned_abs() <= crate::SAFE_INTEGER_MAX.unsigned_abs()),
        Scalar::U53 => value
            .as_u64()
            .is_some_and(|n| n <= crate::SAFE_INTEGER_MAX.unsigned_abs()),
        Scalar::I64 => value.as_i64().is_some(),
        Scalar::U64 => value.as_u64().is_some(),
    };
    if ok {
        return Ok(());
    }
    // An integer of the right sign that is merely too large gets the reason
    // rather than the bare type name: `-1` is not a `u53` for a different
    // reason than 2^60 is, and only one of the two is about the safe range.
    let too_large = match scalar {
        Scalar::I53 => value.as_i64().is_some(),
        Scalar::U53 => value.as_u64().is_some(),
        _ => false,
    };
    if too_large {
        return Err(Error::new(format!(
            "{owner} is `{value}`, which is outside `{scalar}`'s exact range of ±{max} — a \
             value needing more than that is an `i64` or a `u64`, and declaring it so is what \
             tells an implementor whose numbers have the same limit that it has a problem",
            max = crate::SAFE_INTEGER_MAX,
        )));
    }
    Err(Error::new(format!(
        "{owner} is `{value}`, which is not a `{scalar}`"
    )))
}

/// A type that contains itself by value has no size, in Rust and in most
/// targets that generate records. A list breaks the cycle, so the error says
/// so rather than leaving the author to work out why the generated code will
/// not compile.
fn check_no_direct_recursion(scope: &Scope<'_>) -> Result<(), Error> {
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
                    if let Some(Schema::Named(name)) = &variant.payload {
                        out.push(name.as_str());
                    }
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
    for name in scope.types.keys().copied() {
        walk(name, &scope.types, &mut Vec::new(), &mut done)?;
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
