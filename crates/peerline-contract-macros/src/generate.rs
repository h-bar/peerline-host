//! Turning a validated manifest into Rust.
//!
//! Nothing here decides anything a manifest left open: the manifest is
//! validated before this module sees it, so every reference resolves, every
//! description exists, and every op's kind agrees with its shape. What is
//! left is one mapping — manifest spelling to Rust spelling — and it is the
//! same mapping a checker in another language would make into its own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use peerline_contract_spec::{
    ErrorDef, Field, Manifest, OpDef, OpKind, Prose, Scalar, Schema, TypeDef, pascal_case,
};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Ident, LitStr};

/// The whole expansion, in the order it reads best: what the contract is,
/// what its ops are, what can go wrong, and what shapes travel.
pub(crate) fn contract(manifest: &Manifest, path: &Path, span: Span) -> syn::Result<TokenStream> {
    let types = manifest.type_map();

    let header = header(manifest, span);
    let ops = ops(manifest, span)?;
    let errors = errors(manifest, span);
    let shapes = manifest
        .types
        .iter()
        .map(|def| shape(def, &types, span))
        .collect::<syn::Result<Vec<_>>>()?;
    let tracker = rebuild_tracker(path, span);

    Ok(quote! {
        #header
        #ops
        #errors
        #(#shapes)*
        #tracker
    })
}

fn header(manifest: &Manifest, span: Span) -> TokenStream {
    let name = LitStr::new(&manifest.contract, span);
    let version = manifest.version;
    let contract_docs = docs(&manifest.description);
    let version_docs = doc_lines(&[
        format!(
            "The major version of `{}` this expansion describes.",
            manifest.contract
        ),
        String::new(),
        "One integer, additive within it: a breaking change is a new version and a new manifest, \
         never an edit to this one."
            .into(),
    ]);
    quote! {
        #contract_docs
        pub const CONTRACT: &str = #name;

        #version_docs
        pub const VERSION: u32 = #version;
    }
}

fn ops(manifest: &Manifest, span: Span) -> syn::Result<TokenStream> {
    let errors = manifest.error_map();
    let mut consts = Vec::new();
    let mut names = Vec::new();
    let mut kinds = Vec::new();

    for op in &manifest.ops {
        let ident = ident(
            &peerline_contract_spec::const_name("OP", op.short_name(&manifest.contract)),
            span,
        )?;
        let literal = LitStr::new(&op.name, span);
        let prose = docs(&op.description);
        let summary = doc_lines(&op_summary(op, &errors));
        consts.push(quote! {
            #prose
            #summary
            pub const #ident: &str = #literal;
        });
        names.push(quote! { #ident });
        let kind = match op.kind {
            OpKind::Unary => quote!(::peerline_contract::spec::OpKind::Unary),
            OpKind::Stream => quote!(::peerline_contract::spec::OpKind::Stream),
        };
        kinds.push(quote! { (#ident, #kind) });
    }

    let ops_docs = doc_lines(&[
        "Every op of this contract, in the manifest's order.".into(),
        String::new(),
        "This is the list a producer hands to `Produced::with_ops`, so an op missing from it is \
         an op no consumer can discover."
            .into(),
    ]);
    let kinds_docs = doc_lines(&[
        "Every op paired with whether it is unary or a stream.".into(),
        String::new(),
        "Unary and stream are different peerline calls: a producer that registers a stream op as \
         unary fails on the first call with a kind mismatch, and no argument or result schema \
         would have caught it. This table is the manifest's answer, in a form a registration test \
         can check against."
            .into(),
    ]);

    Ok(quote! {
        #(#consts)*

        #ops_docs
        pub const OPS: &[&str] = &[#(#names),*];

        #kinds_docs
        pub const OP_KINDS: &[(&str, ::peerline_contract::spec::OpKind)] = &[#(#kinds),*];
    })
}

/// The one part of an op's documentation that is *not* the manifest's prose:
/// a line naming the Rust spellings a caller needs. The manifest cannot write
/// it, because the names are this generator's.
fn op_summary(op: &OpDef, errors: &BTreeMap<&str, &ErrorDef>) -> Vec<String> {
    let mut parts = vec![format!("Kind: {}.", op.kind)];
    parts.push(match &op.args {
        Some(args) => format!("Args: `{}`.", rust_type_name(args)),
        None => "Args: none.".into(),
    });
    match op.kind {
        OpKind::Unary => parts.push(match &op.result {
            Some(result) => format!("Result: `{}`.", rust_type_name(result)),
            None => "Result: empty.".into(),
        }),
        OpKind::Stream => {
            if let Some(item) = &op.item {
                parts.push(format!("Items: `{}`.", rust_type_name(item)));
            }
        }
    }
    let mut lines = vec![String::new(), parts.join(" ")];
    if !op.errors.is_empty() {
        let raised: Vec<String> = op
            .errors
            .iter()
            .filter_map(|name| errors.get(name.as_str()))
            .map(|e| format!("`{}`", e.const_name()))
            .collect();
        lines.push(format!("Raises: {}.", raised.join(", ")));
    }
    lines
}

fn errors(manifest: &Manifest, span: Span) -> TokenStream {
    let consts: Vec<TokenStream> = manifest
        .errors
        .iter()
        .map(|err| {
            let ident = format_ident!("{}", err.const_name(), span = span);
            let code = err.code;
            let prose = docs(&err.description);
            let carried = match &err.data {
                Some(data) => doc_lines(&[
                    String::new(),
                    format!("Carried in `RpcError::data` as `{}`.", rust_type_name(data)),
                ]),
                None => TokenStream::new(),
            };
            quote! {
                #prose
                #carried
                pub const #ident: i32 = #code;
            }
        })
        .collect();
    quote! { #(#consts)* }
}

fn shape(def: &TypeDef, types: &BTreeMap<&str, &TypeDef>, span: Span) -> syn::Result<TokenStream> {
    match def {
        TypeDef::Struct {
            name,
            description,
            fields,
        } => {
            let ty = ident(name, span)?;
            let prose = docs(description);
            let decls = fields
                .iter()
                .map(|f| field_decl(f, span, Visibility::Public))
                .collect::<syn::Result<Vec<_>>>()?;
            let eq = eq_derive(&fields_schemas(fields), types);
            let ctor = constructor(&ty, name, fields, span)?;
            Ok(quote! {
                #prose
                #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq, #eq ::serde::Serialize, ::serde::Deserialize)]
                pub struct #ty { #(#decls),* }
                #ctor
            })
        }
        TypeDef::Enum {
            name,
            description,
            variants,
        } => {
            let ty = ident(name, span)?;
            let prose = docs(description);
            let arms: Vec<TokenStream> = variants
                .iter()
                .map(|v| {
                    let arm = format_ident!("{}", pascal_case(&v.name), span = span);
                    let wire = LitStr::new(&v.name, span);
                    let prose = docs(&v.description);
                    quote! {
                        #prose
                        #[serde(rename = #wire)]
                        #arm
                    }
                })
                .collect();
            Ok(quote! {
                #prose
                #[derive(
                    ::core::fmt::Debug, ::core::clone::Clone, ::core::marker::Copy,
                    ::core::cmp::PartialEq, ::core::cmp::Eq, ::core::hash::Hash,
                    ::serde::Serialize, ::serde::Deserialize
                )]
                #[non_exhaustive]
                pub enum #ty { #(#arms),* }
            })
        }
        TypeDef::Union {
            name,
            description,
            tag,
            variants,
        } => {
            let ty = ident(name, span)?;
            let prose = docs(description);
            let tag_literal = LitStr::new(tag, span);
            let mut arms = Vec::new();
            let mut ctors = Vec::new();
            let mut schemas = Vec::new();
            for v in variants {
                let arm = format_ident!("{}", pascal_case(&v.name), span = span);
                let wire = LitStr::new(&v.name, span);
                let prose = docs(&v.description);
                schemas.extend(fields_schemas(&v.fields));
                if v.fields.is_empty() {
                    arms.push(quote! {
                        #prose
                        #[serde(rename = #wire)]
                        #arm
                    });
                } else {
                    let decls = v
                        .fields
                        .iter()
                        .map(|f| field_decl(f, span, Visibility::Inherited))
                        .collect::<syn::Result<Vec<_>>>()?;
                    arms.push(quote! {
                        #prose
                        #[serde(rename = #wire)]
                        #arm { #(#decls),* }
                    });
                }
                if let Some(ctor) = variant_constructor(&ty, &arm, v, span)? {
                    ctors.push(ctor);
                }
            }
            let eq = eq_derive(&schemas, types);
            let impl_block = if ctors.is_empty() {
                TokenStream::new()
            } else {
                quote! { impl #ty { #(#ctors)* } }
            };
            Ok(quote! {
                #prose
                #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq, #eq ::serde::Serialize, ::serde::Deserialize)]
                #[serde(tag = #tag_literal)]
                #[non_exhaustive]
                pub enum #ty { #(#arms),* }
                #impl_block
            })
        }
    }
}

fn field_decl(field: &Field, span: Span, vis: Visibility) -> syn::Result<TokenStream> {
    let name = ident(&field.name, span)?;
    let ty = rust_type(&field.ty, span);
    let prose = docs(&field.description);
    // A struct's fields are `pub`; an enum variant's inherit the enum's
    // visibility and may not be spelled.
    let vis = match vis {
        Visibility::Public => quote! { pub },
        Visibility::Inherited => TokenStream::new(),
    };
    if field.optional {
        // Absent, not null: `skip_serializing_if` is what makes an omitted
        // optional field omitted on the way out too, which is the form the
        // manifest describes and the form another language's decoder expects.
        Ok(quote! {
            #prose
            #[serde(default, skip_serializing_if = "::core::option::Option::is_none")]
            #vis #name: ::core::option::Option<#ty>
        })
    } else {
        Ok(quote! {
            #prose
            #vis #name: #ty
        })
    }
}

/// Whether a generated field may be spelled `pub`.
#[derive(Clone, Copy)]
enum Visibility {
    /// A struct field.
    Public,
    /// An enum variant's field, which takes the enum's own visibility.
    Inherited,
}

/// A struct's constructor, taking exactly what the shape requires.
///
/// Generated only when there is something to require: a `new()` that takes
/// nothing says less than the literal it replaces, and trips
/// `clippy::new_without_default` on the way.
fn constructor(
    ty: &Ident,
    name: &str,
    fields: &[Field],
    span: Span,
) -> syn::Result<Option<TokenStream>> {
    let required: Vec<&Field> = fields.iter().filter(|f| !f.optional).collect();
    if required.is_empty() {
        return Ok(None);
    }
    let (params, inits) = ctor_parts(&required, fields, span)?;
    let mut lines = vec![format!("A `{name}`, given everything it requires.")];
    if required.len() < fields.len() {
        lines.push(String::new());
        lines.push("Its optional fields start absent.".into());
    }
    let prose = doc_lines(&lines);
    Ok(Some(quote! {
        impl #ty {
            #prose
            #[must_use]
            #[allow(clippy::too_many_arguments)]
            pub fn new(#(#params),*) -> Self {
                Self { #(#inits),* }
            }
        }
    }))
}

/// One constructor per union variant, named after the variant.
fn variant_constructor(
    ty: &Ident,
    arm: &Ident,
    variant: &peerline_contract_spec::UnionVariant,
    span: Span,
) -> syn::Result<Option<TokenStream>> {
    let required: Vec<&Field> = variant.fields.iter().filter(|f| !f.optional).collect();
    let name = ident(&variant.name, span)?;
    if variant.fields.is_empty() {
        let prose = doc_lines(&[format!("The `{}` variant of `{ty}`.", variant.name)]);
        return Ok(Some(quote! {
            #prose
            #[must_use]
            pub fn #name() -> Self { Self::#arm }
        }));
    }
    let (params, inits) = ctor_parts(&required, &variant.fields, span)?;
    let mut lines = vec![format!(
        "The `{}` variant of `{ty}`, given everything it requires.",
        variant.name
    )];
    if required.len() < variant.fields.len() {
        lines.push(String::new());
        lines.push("Its optional fields start absent.".into());
    }
    let prose = doc_lines(&lines);
    Ok(Some(quote! {
        #prose
        #[must_use]
        #[allow(clippy::too_many_arguments)]
        pub fn #name(#(#params),*) -> Self {
            Self::#arm { #(#inits),* }
        }
    }))
}

type CtorParts = (Vec<TokenStream>, Vec<TokenStream>);

fn ctor_parts(required: &[&Field], all: &[Field], span: Span) -> syn::Result<CtorParts> {
    let mut params = Vec::new();
    for field in required {
        let name = ident(&field.name, span)?;
        // A string-ish parameter takes anything convertible, because every
        // call site otherwise writes `.to_string()` at it.
        let param = match &field.ty {
            Schema::Named(n) if Scalar::from_keyword(n) == Some(Scalar::String) => {
                quote! { #name: impl ::core::convert::Into<::std::string::String> }
            }
            Schema::Named(n) if Scalar::from_keyword(n) == Some(Scalar::Path) => {
                quote! { #name: impl ::core::convert::Into<::std::path::PathBuf> }
            }
            other => {
                let ty = rust_type(other, span);
                quote! { #name: #ty }
            }
        };
        params.push(param);
    }
    let mut inits = Vec::new();
    for field in all {
        let name = ident(&field.name, span)?;
        if field.optional {
            inits.push(quote! { #name: ::core::option::Option::None });
        } else {
            match &field.ty {
                Schema::Named(n)
                    if matches!(Scalar::from_keyword(n), Some(Scalar::String | Scalar::Path)) =>
                {
                    inits.push(quote! { #name: #name.into() });
                }
                _ => inits.push(quote! { #name }),
            }
        }
    }
    Ok((params, inits))
}

fn fields_schemas(fields: &[Field]) -> Vec<Schema> {
    fields.iter().map(|f| f.ty.clone()).collect()
}

/// `Eq` where the shape admits it.
///
/// Not a cosmetic derive: a type that can be a `HashMap` key or compared with
/// `assert_eq!` on both halves of a seam is more useful than one that cannot,
/// and the only thing that can rule it out is a `f64` or a passthrough `json`
/// somewhere in the shape. The manifest already says which, so the derive
/// follows from it rather than from somebody remembering.
fn eq_derive(schemas: &[Schema], types: &BTreeMap<&str, &TypeDef>) -> TokenStream {
    let mut seen = BTreeSet::new();
    if schemas.iter().all(|s| exact(s, types, &mut seen)) {
        quote! { ::core::cmp::Eq, }
    } else {
        TokenStream::new()
    }
}

fn exact(schema: &Schema, types: &BTreeMap<&str, &TypeDef>, seen: &mut BTreeSet<String>) -> bool {
    match schema {
        Schema::Array { array } => exact(array, types, seen),
        Schema::Named(name) => {
            if let Some(scalar) = Scalar::from_keyword(name) {
                return scalar.is_exact();
            }
            if !seen.insert(name.clone()) {
                return true;
            }
            match types.get(name.as_str()) {
                Some(TypeDef::Struct { fields, .. }) => {
                    fields.iter().all(|f| exact(&f.ty, types, seen))
                }
                Some(TypeDef::Union { variants, .. }) => variants
                    .iter()
                    .all(|v| v.fields.iter().all(|f| exact(&f.ty, types, seen))),
                Some(TypeDef::Enum { .. }) => true,
                None => true,
            }
        }
    }
}

fn rust_type(schema: &Schema, span: Span) -> TokenStream {
    match schema {
        Schema::Array { array } => {
            let inner = rust_type(array, span);
            quote! { ::std::vec::Vec<#inner> }
        }
        Schema::Named(name) => match Scalar::from_keyword(name) {
            Some(Scalar::String) => quote! { ::std::string::String },
            Some(Scalar::Bool) => quote! { bool },
            Some(Scalar::I32) => quote! { i32 },
            Some(Scalar::U32) => quote! { u32 },
            Some(Scalar::I64) => quote! { i64 },
            Some(Scalar::U64) => quote! { u64 },
            Some(Scalar::F64) => quote! { f64 },
            Some(Scalar::Path) => quote! { ::std::path::PathBuf },
            Some(Scalar::Json) => quote! { ::serde_json::Value },
            None => {
                let ident = format_ident!("{}", name, span = span);
                quote! { #ident }
            }
        },
    }
}

/// The same mapping, rendered for a doc comment rather than for the compiler.
fn rust_type_name(schema: &Schema) -> String {
    match schema {
        Schema::Array { array } => format!("Vec<{}>", rust_type_name(array)),
        Schema::Named(name) => match Scalar::from_keyword(name) {
            Some(Scalar::String) => "String".into(),
            Some(Scalar::Path) => "PathBuf".into(),
            Some(Scalar::Json) => "serde_json::Value".into(),
            Some(other) => other.keyword().into(),
            None => name.clone(),
        },
    }
}

/// The manifest's prose, as doc comments.
///
/// Line by line rather than as one blob, so a paragraph break in the manifest
/// is a paragraph break in the rendered docs.
fn docs(prose: &Prose) -> TokenStream {
    let lines: Vec<String> = prose.lines().iter().map(|l| (*l).to_string()).collect();
    doc_lines(&lines)
}

fn doc_lines(lines: &[String]) -> TokenStream {
    let attrs: Vec<TokenStream> = lines
        .iter()
        .map(|line| {
            let text = if line.is_empty() {
                String::new()
            } else {
                format!(" {line}")
            };
            quote! { #[doc = #text] }
        })
        .collect();
    quote! { #(#attrs)* }
}

/// The dependency edge that makes a manifest edit a rebuild.
///
/// See the crate docs: without it the compiled types outlive the file they
/// came from, which is the one failure this design cannot tolerate.
fn rebuild_tracker(path: &Path, span: Span) -> TokenStream {
    let literal = LitStr::new(&path.to_string_lossy(), span);
    quote! {
        const _: &[u8] = include_bytes!(#literal);
    }
}

/// A manifest name as a Rust identifier.
///
/// A name that is a Rust keyword is not a manifest error — the manifest is
/// not a Rust file, and another target would take it without complaint — so
/// it becomes a raw identifier, which serialises under its own spelling and
/// so keeps the wire form the manifest declared. The five that cannot even be
/// raw are reported against this target by name.
fn ident(name: &str, span: Span) -> syn::Result<Ident> {
    if let Ok(mut id) = syn::parse_str::<Ident>(name) {
        id.set_span(span);
        return Ok(id);
    }
    if matches!(name, "crate" | "self" | "Self" | "super" | "_") {
        return Err(syn::Error::new(
            span,
            format!(
                "`{name}` cannot be spelled as a Rust identifier, even raw; Rust needs it \
                 renamed in the manifest"
            ),
        ));
    }
    Ok(Ident::new_raw(name, span))
}

#[cfg(test)]
mod tests {
    //! What the expansion *says*, as tokens.
    //!
    //! Reachable only from here: a doc comment leaves no runtime trace, so
    //! the only place the manifest's prose can be checked to have arrived is
    //! against the token stream, before it is handed to the compiler.

    use super::*;

    const FIXTURE: &str = r#"{
        "contract": "demo.thing", "version": 1,
        "description": "Why this contract exists.",
        "types": [
            { "form": "struct", "name": "Args", "description": "What a call carries.",
              "fields": [
                { "name": "id", "type": "string", "description": "Which one." },
                { "name": "extra", "type": "json", "optional": true,
                  "description": "Anything else." } ] },
            { "form": "enum", "name": "Level", "description": "How bad.",
              "variants": [ { "name": "gone_away", "description": "It left." } ] },
            { "form": "union", "name": "Note", "tag": "kind", "description": "One item.",
              "variants": [ { "name": "line", "description": "Something was said.",
                              "fields": [ { "name": "text", "type": "string",
                                            "description": "What was said." } ] } ] }
        ],
        "errors": [ { "name": "demo", "code": 9, "data": "Args",
                      "description": "It did not work." } ],
        "ops": [
            { "name": "demo.thing.go", "kind": "unary", "args": "Args",
              "description": "Do the thing." },
            { "name": "demo.thing.watch", "kind": "stream", "args": "Args", "item": "Note",
              "errors": ["demo"], "description": "Watch the thing." }
        ]
    }"#;

    fn expansion() -> String {
        let manifest = Manifest::parse(FIXTURE).expect("the fixture is valid");
        contract(&manifest, Path::new("/tmp/demo.v1.json"), Span::call_site())
            .expect("the fixture expands")
            .to_string()
    }

    /// The reasoning moves. A generator that dropped the manifest's prose
    /// would leave the contract's documentation in a file nobody reading the
    /// Rust ever opens, which is the failure this whole format exists to fix.
    #[test]
    fn every_description_in_the_manifest_becomes_a_doc_comment() {
        let tokens = expansion();
        for expected in [
            "Why this contract exists.",
            "What a call carries.",
            "Which one.",
            "Anything else.",
            "How bad.",
            "It left.",
            "One item.",
            "Something was said.",
            "What was said.",
            "It did not work.",
            "Do the thing.",
            "Watch the thing.",
        ] {
            assert!(
                tokens.contains(&format!("doc = \" {expected}\"")),
                "`{expected}` did not reach the expansion",
            );
        }
    }

    /// The kind is in the docs as well as in the table, because the first
    /// thing a caller needs to know about an op is which call to make.
    #[test]
    fn an_ops_docs_name_its_kind_and_its_shapes() {
        let tokens = expansion();
        assert!(tokens.contains("Kind: unary. Args: `Args`. Result: empty."));
        assert!(tokens.contains("Kind: stream. Args: `Args`. Items: `Note`."));
        assert!(tokens.contains("Raises: `ERR_DEMO`."));
    }

    /// Without this the compiled types outlive the file they came from — the
    /// one failure a design whose claim is "they cannot drift" cannot afford.
    #[test]
    fn the_expansion_declares_the_manifest_as_a_source_input() {
        let tokens = expansion();
        assert!(tokens.contains("include_bytes"), "{tokens}");
        assert!(tokens.contains("\"/tmp/demo.v1.json\""), "{tokens}");
    }

    /// A contract version is additive, so a reader must tolerate a value it
    /// has not heard of. Structs are not marked, because a caller has to be
    /// able to write one out field by field.
    #[test]
    fn enums_are_open_and_structs_are_not() {
        let tokens = expansion();
        assert_eq!(
            tokens.matches("non_exhaustive").count(),
            2,
            "one for each of `Level` and `Note`, and none for `Args`: {tokens}",
        );
    }

    /// An optional field is absent on the way out, not null.
    #[test]
    fn optional_fields_are_omitted_rather_than_nulled() {
        assert!(expansion().contains("skip_serializing_if = \"::core::option::Option::is_none\""));
    }

    /// A manifest is not a Rust file, and another target would take a name
    /// Rust reserves without complaint — so Rust takes it too, as a raw
    /// identifier, and the wire name is unchanged.
    #[test]
    fn a_field_named_after_a_rust_keyword_becomes_a_raw_identifier() {
        let json = FIXTURE.replace(r#""name": "id""#, r#""name": "type""#);
        let manifest = Manifest::parse(&json).expect("`type` is a fine wire name");
        let tokens = contract(&manifest, Path::new("/tmp/demo.v1.json"), Span::call_site())
            .expect("expands")
            .to_string();
        assert!(tokens.contains("r#type"), "{tokens}");
    }
}
