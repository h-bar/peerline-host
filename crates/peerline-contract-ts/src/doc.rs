//! The manifest's prose, as TypeScript doc comments.
//!
//! Every description in a manifest is required, and the reason given in the
//! format's own docs is that the prose *is* the contract documentation — read
//! by whoever implements the contract, in whatever language, and emitted by
//! each generator as comments in its own target. This is that, for TypeScript.
//!
//! # Why JSDoc block comments and not `//`
//!
//! A `/** … */` immediately before a declaration or a member is what every
//! TypeScript editor shows on hover, and what `tsc --declaration` copies into
//! the `.d.ts` it emits. A `//` line comment is shown by neither. Since the
//! whole point is that the reasoning reaches the person writing the consumer,
//! the form that reaches them is the one to use.
//!
//! # The prose is plain, and stays plain
//!
//! The format rejects rustdoc `[`Foo`]` and markdown `[text](url)` before a
//! manifest parses, so nothing here has to strip either. What is left is
//! plain text, and it is emitted as plain text: no reflowing, no
//! re-wrapping, no markdown interpretation. The author's line breaks are the
//! author's, and a paragraph break written as a blank entry stays one.

use peerline_contract_spec::Prose;

/// Render prose as a JSDoc block, indented by `indent` spaces, with a
/// trailing newline. Empty prose renders nothing — which a validated manifest
/// never has, but a caller assembling a comment out of several pieces can.
pub(crate) fn block(prose: &Prose, indent: usize) -> String {
    lines_block(&prose.lines(), indent)
}

/// [`block`], for lines assembled from more than one source — a field's own
/// description plus the note about its default, say.
pub(crate) fn lines_block(lines: &[&str], indent: usize) -> String {
    let trimmed = trim_blank_edges(lines);
    if trimmed.is_empty() {
        return String::new();
    }
    let pad = " ".repeat(indent);
    let mut out = format!("{pad}/**\n");
    for line in trimmed {
        let line = escape(line);
        if line.is_empty() {
            // No trailing space on a blank continuation line: it is invisible
            // in every editor and visible in every diff.
            out.push_str(&format!("{pad} *\n"));
        } else {
            out.push_str(&format!("{pad} * {line}\n"));
        }
    }
    out.push_str(&format!("{pad} */\n"));
    out
}

/// Strip leading and trailing blank lines, which would render as an empty
/// ` *` at the top or bottom of the block and say nothing.
fn trim_blank_edges<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let Some(start) = start else {
        return Vec::new();
    };
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .unwrap_or(start);
    lines[start..=end].iter().map(|l| l.trim_end()).collect()
}

/// Make one line safe inside a block comment.
///
/// `*/` in prose would close the comment early and turn the rest of the
/// description into a syntax error. The format does not forbid the sequence —
/// it is a fact about C-family comments, not about manifests, and a Python
/// generator has no use for the rule — so it is handled here rather than
/// being pushed into the shared file as a restriction other targets would
/// have to honour for no reason.
fn escape(line: &str) -> String {
    line.replace("*/", "*\\/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_entry_is_a_paragraph_break_and_carries_no_trailing_space() {
        let prose = Prose::Lines(vec!["First.".into(), String::new(), "Second.".into()]);
        assert_eq!(block(&prose, 0), "/**\n * First.\n *\n * Second.\n */\n");
    }

    /// A `Line` holding newlines means the same as the list form; the format
    /// says so, and a generator that treated them differently would render one
    /// manifest two ways depending on how its author happened to type it.
    #[test]
    fn the_two_prose_forms_render_identically() {
        let one = Prose::Line("First.\n\nSecond.".into());
        let many = Prose::Lines(vec!["First.".into(), String::new(), "Second.".into()]);
        assert_eq!(block(&one, 0), block(&many, 0));
    }

    #[test]
    fn indentation_reaches_every_line_of_the_block() {
        let prose = Prose::Line("Only.".into());
        assert_eq!(block(&prose, 2), "  /**\n   * Only.\n   */\n");
    }

    /// Prose is not TypeScript's to parse, but a comment terminator in it is
    /// TypeScript's problem.
    #[test]
    fn a_comment_terminator_in_prose_cannot_close_the_block() {
        let prose = Prose::Line("Matches the glob a*/b.".into());
        let rendered = block(&prose, 0);
        assert!(rendered.contains("a*\\/b"), "{rendered}");
        assert_eq!(
            rendered.matches("*/").count(),
            1,
            "exactly one terminator, the real one: {rendered}"
        );
    }

    #[test]
    fn blank_edges_are_trimmed_rather_than_rendered_as_empty_lines() {
        let prose = Prose::Lines(vec![String::new(), "Body.".into(), String::new()]);
        assert_eq!(block(&prose, 0), "/**\n * Body.\n */\n");
    }
}
