//! The one place a manifest's spelling of a scalar becomes TypeScript's.
//!
//! # This table is a second copy, and that is deliberate
//!
//! `peerline-contract-macros` carries a `ScalarTarget` table with four
//! columns, one of which is a TypeScript override — the string it hands
//! `#[ts(type = "…")]` when `ts_rs` reading the generated *Rust* would get the
//! answer wrong. That column is not this table, and could not be: it is a
//! patch applied to somebody else's guess, so it has an entry only where the
//! guess is wrong and says nothing at all for the eight rows `ts_rs` happens
//! to get right. Reading TypeScript off it means knowing what `ts_rs` does
//! with a `String`, a `bool` and a `PathBuf`, which is knowledge held in a
//! third crate that is not a dependency of either of these.
//!
//! This table has no such gap: every row says what the keyword *is* in
//! TypeScript, and the two agree everywhere the macros crate has an opinion.
//! `this_table_agrees_with_the_macro_crates_typescript_column`, in this
//! module's own test block, is that agreement written down as a test. Named
//! in text rather than linked: it is a `cfg(test)` item, so a link to it
//! would resolve in a test build and dangle in the documentation anybody
//! actually reads.
//!
//! **The manual-sync relationship.** The macros crate is not a dependency of
//! this one and cannot be — it is a proc-macro crate, and a proc-macro crate
//! exports nothing but macros, so its `ScalarTarget` is unreachable from here
//! by construction rather than by choice. So the two tables are kept in step
//! by hand, and the thing that makes that safe is [`Scalar`] being a closed
//! enum: a twelfth keyword added to the format makes the match in
//! [`scalar`] a compile error here, and the same in the macros crate there.
//! Neither table can silently miss a row. What a human still has to do is
//! notice when the macros crate *changes* an existing row's `ts` column, and
//! the cross-check test is what fails when they do not.
//!
//! The loop is closed from the other side too: `ScalarTarget::ts` in
//! `peerline-contract-macros` names this module and says that a new scalar
//! edits both tables. The two comments point at each other on purpose, so
//! that whichever one a reader reaches first tells them the other exists.
//!
//! # What this table is *not* asked about
//!
//! Only leaves. How a list, a map or a nullable wraps its inner answer is
//! [`crate::render`]'s, and the macros crate's `ts_override` makes the
//! matching decision for its own attribute — `Array<…>`, `Record<string, …>`,
//! `… | null` — with the same spellings, for the same reason the leaf answers
//! must match. A downstream diffing the two generators has to see one shape
//! described one way, and a composite spelled differently would be a
//! difference that is not one.

use peerline_contract_spec::Scalar;

/// Whether the two 64-bit keywords are emitted as `bigint` or as `number`.
///
/// This is the one mapping question the manifest does not settle on its own,
/// because both answers are lossy in different directions and which loss is
/// acceptable is a fact about the consumer rather than about the contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WideIntegers {
    /// `i64` and `u64` are `bigint`. The default, and the compatible answer:
    /// it is what `ts_rs` emits for the Rust those keywords generate today,
    /// and the macros crate leaves it alone on purpose — see its note that
    /// "for those the `bigint` is the truth and the warning".
    ///
    /// It is also the honest one. A value past 2^53 − 1 is not a JavaScript
    /// number, and `JSON.parse` will not produce a `bigint` for it: a consumer
    /// holding one of these has to reach for a reviver or a
    /// `json-bigint`-shaped parser, and the type is what tells them so at the
    /// keyboard rather than in production. The friction is the feature.
    #[default]
    Bigint,
    /// `i64` and `u64` are `number`.
    ///
    /// For a consumer that has looked at the values it actually receives and
    /// concluded they are inside the safe range — a migration in progress, or
    /// a contract whose `i64` predates `i53` existing. It is a promise this
    /// crate cannot check and the manifest explicitly declined to make, so it
    /// is opt-in and it is not the default.
    ///
    /// The better fix is upstream: if the values really are safe, the manifest
    /// should say `i53`, and then every target learns it rather than this one.
    Number,
}

/// The TypeScript a scalar keyword is.
///
/// Exhaustive over [`Scalar`] on purpose: see the module docs. Adding a
/// keyword to the format is a compile error here, which is the whole
/// protection a hand-synced table gets.
pub(crate) fn scalar(scalar: Scalar, wide: WideIntegers) -> &'static str {
    match scalar {
        // Text is text.
        Scalar::String => "string",
        Scalar::Bool => "boolean",
        // Every integer whose range a double represents exactly. `i32` and
        // `u32` trivially; `i53` and `u53` because ±(2^53 − 1) *is* the exact
        // range of a double, which is the whole content of those two
        // keywords. The macros crate spends an override on the second pair
        // for the same reason, having had to route them through a Rust `i64`.
        Scalar::I32 | Scalar::U32 | Scalar::I53 | Scalar::U53 => "number",
        // The pair the format grew `i53` in order to be able to mean. See
        // `WideIntegers`.
        Scalar::I64 | Scalar::U64 => match wide {
            WideIntegers::Bigint => "bigint",
            WideIntegers::Number => "number",
        },
        Scalar::F64 => "number",
        // A path is a string on the wire, and TypeScript has no filesystem
        // type to promote it to. What `path` carries over `string` is *where
        // it is resolved*, which is a fact the prose states and no type
        // expresses — so the type is `string` and the prose travels with it
        // as the doc comment, which is where a reader will find it.
        Scalar::Path => "string",
        // Passthrough JSON. `unknown` rather than `any`: the manifest says
        // the value is not interpreted, and `unknown` is TypeScript's way of
        // saying a value must be narrowed before it is used. `any` would say
        // the opposite — that it may be used as anything without a check —
        // which is not what an uninterpreted value is.
        Scalar::Json => "unknown",
    }
}

/// The TypeScript override the macros crate carries for this keyword, if any.
///
/// A restatement of that crate's `ScalarTarget::ts` column, kept here so the
/// cross-check test has something to compare [`scalar`] against without
/// depending on a proc-macro crate. See the module docs.
#[cfg(test)]
pub(crate) fn macros_crate_override(scalar: Scalar) -> Option<&'static str> {
    match scalar {
        Scalar::I53 | Scalar::U53 => Some("number"),
        Scalar::Json => Some("unknown"),
        Scalar::String
        | Scalar::Bool
        | Scalar::I32
        | Scalar::U32
        | Scalar::I64
        | Scalar::U64
        | Scalar::F64
        | Scalar::Path => None,
    }
}

/// What `ts_rs` emits for the Rust the macros crate generates, where that
/// crate writes no override. The other half of the cross-check: an override
/// of `None` is a claim that `ts_rs` already agrees, and this is what it
/// agrees *to*.
#[cfg(test)]
pub(crate) fn ts_rs_unassisted(scalar: Scalar) -> Option<&'static str> {
    Some(match scalar {
        // `String` and `PathBuf`.
        Scalar::String | Scalar::Path => "string",
        Scalar::Bool => "boolean",
        Scalar::I32 | Scalar::U32 | Scalar::F64 => "number",
        // The 64-bit Rust integers, which is where the warning lives.
        Scalar::I64 | Scalar::U64 => "bigint",
        // These are the two the macros crate has to override, so `ts_rs`
        // unassisted has no agreed answer to compare against.
        Scalar::I53 | Scalar::U53 | Scalar::Json => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every keyword the format has.
    ///
    /// Hand-written, and guarded by
    /// [`the_list_below_is_every_scalar_the_format_has`]: the exhaustive match
    /// there stops compiling the moment a twelfth keyword is added, which is
    /// what sends a human back here to extend the list.
    const ALL: [Scalar; 11] = [
        Scalar::String,
        Scalar::Bool,
        Scalar::I32,
        Scalar::U32,
        Scalar::I53,
        Scalar::U53,
        Scalar::I64,
        Scalar::U64,
        Scalar::F64,
        Scalar::Path,
        Scalar::Json,
    ];

    #[test]
    fn the_list_below_is_every_scalar_the_format_has() {
        for s in ALL {
            // No wildcard arm, deliberately. This match is the tripwire: a
            // keyword added to `peerline-contract-spec` breaks the build here
            // and in `scalar` above, and the second of those is the one that
            // matters — a generator that silently mapped a new keyword to
            // something plausible would be exactly the quiet wrongness the
            // whole format exists to prevent.
            match s {
                Scalar::String
                | Scalar::Bool
                | Scalar::I32
                | Scalar::U32
                | Scalar::I53
                | Scalar::U53
                | Scalar::I64
                | Scalar::U64
                | Scalar::F64
                | Scalar::Path
                | Scalar::Json => {}
            }
        }
        let keywords: BTreeSet<&str> = ALL.iter().map(|s| s.keyword()).collect();
        assert_eq!(keywords.len(), ALL.len(), "two entries share a keyword");
        for s in ALL {
            assert_eq!(
                Scalar::from_keyword(s.keyword()),
                Some(s),
                "`{}` must round-trip through the format's own parser",
                s.keyword()
            );
        }
    }

    /// The cross-check the two hand-synced tables get.
    ///
    /// `peerline-contract-macros` cannot be a dependency of this crate — a
    /// proc-macro crate exports nothing but macros — so its `ScalarTarget`
    /// table is restated here and compared. Where that crate writes an
    /// override, this table must produce the same string; where it writes
    /// none, it is asserting that `ts_rs` already agrees, and this table must
    /// produce what `ts_rs` would have said.
    ///
    /// Compared at [`WideIntegers::Bigint`], which is the compatible setting:
    /// the other one is an opt-out from what the macros crate emits and is
    /// expected to differ.
    #[test]
    fn this_table_agrees_with_the_macro_crates_typescript_column() {
        for s in ALL {
            let mine = scalar(s, WideIntegers::Bigint);
            match macros_crate_override(s) {
                Some(theirs) => assert_eq!(
                    mine,
                    theirs,
                    "`{}`: the macros crate overrides `ts_rs` to `{theirs}` and this crate \
                     says `{mine}`; the two generators would document different contracts",
                    s.keyword()
                ),
                None => {
                    let unassisted = ts_rs_unassisted(s).unwrap_or_else(|| {
                        panic!(
                            "`{}` has no macro override and no recorded ts_rs answer, so \
                             nothing here is checking it",
                            s.keyword()
                        )
                    });
                    assert_eq!(
                        mine,
                        unassisted,
                        "`{}`: the macros crate writes no override, which asserts `ts_rs` \
                         already emits `{unassisted}` for the Rust it generates — and this \
                         crate says `{mine}`",
                        s.keyword()
                    );
                }
            }
        }
    }

    /// The distinction the safe-integer keywords exist to make, stated as a
    /// test so that collapsing the two pairs into one row fails loudly.
    #[test]
    fn the_safe_and_wide_integers_are_not_the_same_type() {
        for wide in [WideIntegers::Bigint, WideIntegers::Number] {
            assert_eq!(scalar(Scalar::I53, wide), "number");
            assert_eq!(scalar(Scalar::U53, wide), "number");
        }
        assert_eq!(scalar(Scalar::I64, WideIntegers::Bigint), "bigint");
        assert_eq!(scalar(Scalar::U64, WideIntegers::Bigint), "bigint");
        assert_eq!(scalar(Scalar::I64, WideIntegers::Number), "number");
        assert_eq!(scalar(Scalar::U64, WideIntegers::Number), "number");
    }

    /// `WideIntegers` reaches only the two keywords it is about.
    #[test]
    fn the_wide_integer_option_touches_nothing_else() {
        for s in ALL {
            if matches!(s, Scalar::I64 | Scalar::U64) {
                continue;
            }
            assert_eq!(
                scalar(s, WideIntegers::Bigint),
                scalar(s, WideIntegers::Number),
                "`{}` must not depend on an option about 64-bit integers",
                s.keyword()
            );
        }
    }
}
