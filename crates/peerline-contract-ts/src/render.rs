//! Turning one manifest's shapes into TypeScript declarations.
//!
//! Every decision this module makes is written down in the crate docs; what is
//! here is the machinery. The one structural idea worth stating at the top is
//! that a shape becomes a [`Decl`] — a name, the other names it mentions, and
//! its rendered text — and that assembling `Decl`s into *files* happens
//! elsewhere. That split is what lets one renderer serve both layouts: whether
//! `Meta` lands in its own file or beside its siblings changes which
//! references need an `import type` line, and changes nothing about the
//! declaration itself.

use std::collections::BTreeSet;

use peerline_contract_spec::{
    Field, FieldState, Prose, Scalar, Schema, Tagging, TypeDef, UnionVariant,
};

use crate::doc;
use crate::scalar::scalar;
use crate::{DefaultFields, Options};

/// One generated `export type`, with the type names it mentions.
pub(crate) struct Decl {
    /// The declared name, which is also the file name under
    /// [`Layout::Type`](crate::Layout::Type).
    pub(crate) name: String,
    /// Every named, non-scalar type this declaration mentions — local or
    /// imported, since which it is depends on the file it lands in.
    pub(crate) refers_to: BTreeSet<String>,
    /// The declaration, doc comment and all, with a trailing newline.
    pub(crate) text: String,
}

/// One member of an object type.
struct Member {
    /// The prose above it, already assembled.
    doc: Vec<String>,
    /// The key, as it is written on the wire. Quoted on the way out when it
    /// is not a legal bare identifier — see [`key`].
    key: String,
    /// Whether the key may be absent, i.e. whether it gets a `?`.
    optional: bool,
    /// The member's type, rendered. May be several lines when it is a nested
    /// object, in which case it is already indented for where it sits.
    ty: String,
}

/// Render every shape in a manifest.
pub(crate) fn decls(types: &[TypeDef], opts: &Options) -> Vec<Decl> {
    types.iter().map(|def| decl(def, opts)).collect()
}

fn decl(def: &TypeDef, opts: &Options) -> Decl {
    let name = def.name().to_string();
    let mut refers_to = BTreeSet::new();
    collect_refs(def, &mut refers_to);
    // A shape that mentions itself needs no import of itself, and under the
    // per-type layout would otherwise emit one.
    refers_to.remove(&name);
    let body = match def {
        TypeDef::Struct { fields, .. } => struct_body(fields, opts),
        // # A plain alias, and not a branded type
        //
        // Rust gets a distinct type for a newtype because Rust's newtypes are
        // free: a wrapper struct costs nothing at run time, and the value
        // comes out with `.0` or an `Into`. TypeScript has no such thing.
        // The nearest equivalent is a *brand* — `string & { __brand: "HostId" }`
        // — and brands are not free here, they are expensive in exactly the
        // place this generator's output is used.
        //
        // Every value of a generated type in TypeScript arrives from
        // `JSON.parse`. A parsed string is a `string`; it is not assignable to
        // a branded `string`, so a consumer would have to cast at every
        // boundary — which is the boundary where the types were supposed to
        // be helping. The brand would also leak into the JSON: `JSON.stringify`
        // of a branded value is fine, but the brand shows up in editor hovers,
        // in `.d.ts` output, and in every error message about a shape that
        // does not have it. What a downstream would get is friction at the
        // seam and no check that is not already made by the producer.
        //
        // So the asymmetry is real and is stated rather than papered over:
        // Rust distinguishes `HostId` from `String`, TypeScript does not, and
        // the manifest's prose is what carries the distinction to a
        // TypeScript reader. That is the same answer `path` already gets —
        // `path` is `string` here too, and what the keyword adds is prose
        // about where the value is resolved.
        TypeDef::Newtype { newtype, .. } => format!(" {}", ty_expr(newtype, opts)),
        TypeDef::Enum { variants, .. } => {
            let arms: Vec<String> = variants
                .iter()
                .map(|v| union_arm(&v.description, &string_literal(v.wire())))
                .collect();
            arms.join("")
        }
        TypeDef::Union {
            tagging,
            tag,
            variants,
            ..
        } => {
            let arms: Vec<String> = variants
                .iter()
                .map(|v| match tagging {
                    Tagging::Internal => internal_arm(v, tag.as_deref().unwrap_or("kind"), opts),
                    Tagging::External => external_arm(v, opts),
                })
                .collect();
            arms.join("")
        }
    };
    let text = format!(
        "{}export type {name} ={body};\n",
        doc::block(def.description(), 0),
    );
    Decl {
        name,
        refers_to,
        text,
    }
}

/// A struct is an object type, intersected with whatever it flattens.
///
/// The intersection is not a stylistic choice: a flattened field's keys are
/// written *beside* its siblings, so the enclosing object genuinely has both
/// sets of keys, and `A & B` is how TypeScript spells a value that has both.
fn struct_body(fields: &[Field], opts: &Options) -> String {
    let flattened: Vec<String> = fields
        .iter()
        .filter(|f| f.flatten)
        .map(|f| ty_expr(&f.ty, opts))
        .collect();
    let members: Vec<Member> = fields
        .iter()
        .filter(|f| !f.flatten)
        .map(|f| member(f, opts))
        .collect();

    if flattened.is_empty() {
        return format!(" {}", object(&members, 0));
    }
    // Every field is flattened, so the object half would be an empty `{}` —
    // which in TypeScript means "any non-nullish value" and would widen the
    // intersection to nothing useful. The shape *is* the things it flattens.
    if members.is_empty() {
        return format!(" {}", flattened.join(" & "));
    }
    format!(" {} & {}", object(&members, 0), flattened.join(" & "))
}

/// One arm of a union, on its own line with the leading `|` that Prettier
/// writes for a multi-line union.
fn union_arm(prose: &Prose, body: &str) -> String {
    format!("\n{}  | {body}", doc::block(prose, 2))
}

/// An internally tagged arm: the discriminator is a key inside the object,
/// beside the arm's own keys.
///
/// A `payload` arm becomes an intersection because that is what the wire is —
/// the payload's keys sit beside the tag, which is the same object as
/// spelling the fields out. Parenthesised so the `&` cannot be misread as
/// binding across the `|`, which it does not, but a reader should not have to
/// know that.
fn internal_arm(v: &UnionVariant, tag: &str, opts: &Options) -> String {
    let discriminant = Member {
        doc: Vec::new(),
        key: tag.to_string(),
        optional: false,
        ty: string_literal(v.wire()),
    };
    let body = match &v.payload {
        Some(payload) => format!(
            "({{ {tag}: {} }} & {})",
            string_literal(v.wire()),
            ty_expr(payload, opts)
        ),
        None => {
            let mut members = vec![discriminant];
            members.extend(v.fields.iter().map(|f| member(f, opts)));
            object(&members, 4)
        }
    };
    union_arm(&v.description, &body)
}

/// An externally tagged arm: the object has exactly one key, which is the
/// arm's name, and the value is the payload.
///
/// A variant with neither a payload nor fields is a bare string on the wire —
/// serde's external tagging writes a unit variant as its name, not as an
/// object with a null value — so that is what it becomes here.
fn external_arm(v: &UnionVariant, opts: &Options) -> String {
    let body = match (&v.payload, v.fields.is_empty()) {
        (Some(payload), _) => {
            let member = Member {
                doc: Vec::new(),
                key: v.wire().to_string(),
                optional: false,
                ty: ty_expr(payload, opts),
            };
            object(&[member], 4)
        }
        (None, true) => string_literal(v.wire()),
        (None, false) => {
            let inner: Vec<Member> = v.fields.iter().map(|f| member(f, opts)).collect();
            let member = Member {
                doc: Vec::new(),
                key: v.wire().to_string(),
                optional: false,
                // The nested object's braces sit at the member's own column.
                ty: object(&inner, 6),
            };
            object(&[member], 4)
        }
    };
    union_arm(&v.description, &body)
}

/// One field, as an object member — where the three field states are decided.
fn member(field: &Field, opts: &Options) -> Member {
    let base = ty_expr(&field.ty, opts);
    let mut doc: Vec<String> = field
        .description
        .lines()
        .iter()
        .map(|l| l.to_string())
        .collect();
    let (optional, ty) = match field.state() {
        // The key is always written and always has a value.
        FieldState::Required => (false, base),
        // Absent means nothing was said, and a writer omits the key rather
        // than writing null — so `key?: T`, never `key: T | null`. The two
        // are different wire forms and the manifest distinguishes them; this
        // is where the distinction reaches the browser.
        FieldState::Optional => (true, base),
        // The key is always written; `null` is the nothing-to-say value. No
        // `?`, because a writer must emit the key.
        //
        // No generated note here, deliberately. `T | null` without a `?`
        // already says "always written, sometimes null", and the one fact it
        // does not carry — that a decoder should tolerate an absent key
        // anyway — is the same sentence on every nullable field in every
        // contract. Stamping boilerplate above the author's prose dilutes the
        // prose, which is the thing the format went to the trouble of
        // requiring. It is in this crate's docs instead, once.
        FieldState::Nullable => (false, format!("{base} | null")),
        // Absence has a *value*. See the crate docs for why this is `key?:`
        // and not `key: T`.
        FieldState::Default(value) => {
            doc.push(String::new());
            doc.push(format!("Absent means `{}`.", json_text(value)));
            match opts.defaults {
                DefaultFields::Optional => (true, base),
                DefaultFields::Required => (false, base),
            }
        }
        // Absence has a value *and* is legal on the way out. Rendered exactly
        // as the state above — `key?: T`, with the value in the prose —
        // because TypeScript has one type for both directions and these two
        // states differ only in a direction. See the crate docs.
        //
        // `DefaultFields` is deliberately not consulted. That option exists to
        // reproduce what the `ts_rs` route emits for a *plain* default, which
        // is `key: T`, and it is defensible there because a producer of a
        // plainly defaulted field really does write the key every time — the
        // `?` is a widening chosen for the caller's benefit. Here a producer
        // genuinely omits the key, so `key: T` would not be a conservative
        // reading of an ambiguous case; it would be false. An option whose
        // whole purpose is byte-compatibility with an older route has nothing
        // to say about a state that route could not describe at all.
        FieldState::OptionalDefault(value) => {
            doc.push(String::new());
            doc.push(format!("Absent means `{}`.", json_text(value)));
            doc.push(
                "A writer omits the key when the value is that, so an absent key is \
                 something this contract's own producer emits."
                    .to_string(),
            );
            (true, base)
        }
    };
    Member {
        doc,
        key: field.name.clone(),
        optional,
        ty,
    }
}

/// A JSON value as the text a doc comment shows it as.
///
/// The same rendering `peerline-contract-macros` puts in its own generated
/// docs, so that one manifest default reads as one string in both targets.
fn json_text(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

/// A schema, as a TypeScript type expression.
pub(crate) fn ty_expr(schema: &Schema, opts: &Options) -> String {
    match schema {
        Schema::Named(name) => match Scalar::from_keyword(name) {
            Some(s) => scalar(s, opts.wide_integers).to_string(),
            // Not a scalar, so the validator has already established it
            // resolves to a declared or imported type. The format requires
            // PascalCase alphanumeric names, which are legal TypeScript
            // identifiers as they stand.
            None => name.clone(),
        },
        // `Array<T>` rather than `T[]`: the two are identical to the
        // compiler, and the generic form is what the existing bindings use
        // and what stays readable when the element is itself a union.
        Schema::Array { array } => format!("Array<{}>", ty_expr(array, opts)),
        // `Record<string, V>`, which is TypeScript's own spelling for an
        // object whose keys are not enumerated. The key type is `string` and
        // is not read off anything, because a JSON object's keys are strings
        // and the schema says no more than that — there is no key-type
        // keyword in the format and nothing one could say with it.
        //
        // `Record<string, never>` never arises from here. That spelling is
        // what an *empty struct* becomes, and an empty struct is a shape with
        // no keys; a map with no entries is a **value**, not a type, and its
        // type is still `Record<string, V>`. The two look adjacent and are
        // not related at all.
        Schema::Map { map } => format!("Record<string, {}>", ty_expr(map, opts)),
        // A nullable *schema* — the form used where there is no field to
        // carry `nullable`, i.e. an op's result or item or an error's data.
        // The validator forbids it inside an array or a field, so it never
        // needs parenthesising here.
        Schema::Nullable { nullable } => format!("{} | null", ty_expr(nullable, opts)),
    }
}

/// `{ … }` for a set of members, with the braces at column `indent`.
///
/// An object with no members is `Record<string, never>` and not `{}`: in
/// TypeScript `{}` means "anything that is not null or undefined", which is
/// the opposite of a shape with no keys.
fn object(members: &[Member], indent: usize) -> String {
    if members.is_empty() {
        return "Record<string, never>".to_string();
    }
    let pad = " ".repeat(indent);
    let inner = indent + 2;
    let ipad = " ".repeat(inner);
    let mut out = String::from("{\n");
    for m in members {
        let lines: Vec<&str> = m.doc.iter().map(String::as_str).collect();
        out.push_str(&doc::lines_block(&lines, inner));
        let question = if m.optional { "?" } else { "" };
        out.push_str(&format!("{ipad}{}{question}: {};\n", key(&m.key), m.ty));
    }
    out.push_str(&format!("{pad}}}"));
    out
}

/// A key, bare where TypeScript allows it and quoted where it does not.
///
/// A struct field's name is `lower_snake_case` and never needs quoting. An
/// **externally tagged union arm** is the case that does: its key is the arm's
/// *wire* spelling, and a wire value may be any non-blank string — `by-name`,
/// `turn-started` — precisely because the format refuses to impose one
/// language's identifier rules on the wire. Quoting is how this target obeys
/// its own rules without asking the contract to change.
fn key(name: &str) -> String {
    let mut chars = name.chars();
    let bare = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if bare {
        name.to_string()
    } else {
        string_literal(name)
    }
}

/// A wire value as a TypeScript string literal.
///
/// Double quotes, which is what Prettier's default writes and therefore what
/// a repository that runs Prettier over the generated tree will leave alone.
/// A wire value may be any non-blank string, so it is escaped rather than
/// interpolated: `serde_json` produces exactly a double-quoted JSON string,
/// and a JSON string literal is a TypeScript string literal.
pub(crate) fn string_literal(wire: &str) -> String {
    serde_json::to_string(wire).unwrap_or_else(|_| format!("\"{wire}\""))
}

/// Every named, non-scalar type a shape mentions.
fn collect_refs(def: &TypeDef, out: &mut BTreeSet<String>) {
    fn schema(s: &Schema, out: &mut BTreeSet<String>) {
        match s {
            Schema::Named(name) => {
                if Scalar::from_keyword(name).is_none() {
                    out.insert(name.clone());
                }
            }
            Schema::Array { array } => schema(array, out),
            Schema::Map { map } => schema(map, out),
            Schema::Nullable { nullable } => schema(nullable, out),
        }
    }
    fn fields(fs: &[Field], out: &mut BTreeSet<String>) {
        for f in fs {
            schema(&f.ty, out);
        }
    }
    match def {
        TypeDef::Struct { fields: fs, .. } => fields(fs, out),
        // A closed set of strings mentions nothing.
        TypeDef::Enum { .. } => {}
        // An alias mentions exactly what it aliases, which under the per-type
        // and folder layouts is a file it has to import.
        TypeDef::Newtype { newtype, .. } => schema(newtype, out),
        TypeDef::Union { variants, .. } => {
            for v in variants {
                fields(&v.fields, out);
                if let Some(payload) = &v.payload {
                    schema(payload, out);
                }
            }
        }
    }
}
