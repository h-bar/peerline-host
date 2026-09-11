//! Turning a manifest's own spellings into a target's identifier
//! conventions.
//!
//! The manifest writes wire names: `thread_id`, `session_gone`,
//! `codex.server.thread.start`. Every generator has to map those to whatever
//! its language calls a type, a variant and a constant, and the mapping has to
//! be the *same* mapping in each of them — otherwise `session_gone` is
//! `SessionGone` in one binding and `SESSION_GONE` in another and the two
//! bindings document different contracts. So it is defined once, here, rather
//! than per generator.

/// `snake_case` or `dotted.snake_case` to `PascalCase`.
///
/// Dots and underscores are both word separators, so `thread.start` and
/// `thread_start` both give `ThreadStart`. Digits do not start a new word.
pub fn pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = true;
    for ch in name.chars() {
        if ch == '_' || ch == '.' || ch == '-' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// `snake_case` or `dotted.snake_case` to `SCREAMING_SNAKE_CASE`.
pub fn screaming_case(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '.' | '-' => '_',
            other => other.to_ascii_uppercase(),
        })
        .collect()
}

/// The constant name for something, given a prefix: `const_name("OP",
/// "thread.start")` is `"OP_THREAD_START"`.
pub fn const_name(prefix: &str, name: &str) -> String {
    format!("{prefix}_{}", screaming_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_treats_dots_and_underscores_alike() {
        assert_eq!(pascal_case("thread.start"), "ThreadStart");
        assert_eq!(pascal_case("thread_start"), "ThreadStart");
        assert_eq!(pascal_case("session_gone"), "SessionGone");
        assert_eq!(pascal_case("line"), "Line");
    }

    /// The mapping is not allowed to invent word boundaries at digits: an op
    /// called `v2_start` is `V2Start`, not `V2_Start` or `V2start`.
    #[test]
    fn digits_do_not_start_a_word() {
        assert_eq!(pascal_case("v2_start"), "V2Start");
    }

    #[test]
    fn constants_take_the_prefix_and_the_screaming_form() {
        assert_eq!(const_name("OP", "thread.start"), "OP_THREAD_START");
        assert_eq!(const_name("ERR", "codex_server"), "ERR_CODEX_SERVER");
    }
}
