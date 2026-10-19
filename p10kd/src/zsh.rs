//! Rendering values as zsh source text.
//!
//! The daemon's replies are evaluated by the shell, so every fact crosses this one
//! boundary as a single zsh word. [LAW:single-enforcer] quoting lives here only.

use crate::protocol::RECORD_END;

/// Quote `s` as one zsh word that evaluates back to exactly `s`. The word is single
/// quoted; a single quote inside closes, escapes and reopens, and the record terminator
/// is spliced in as `$'\x1e'` so the shell's record splitting never sees it.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("'\\''"),
            c if c as u32 == RECORD_END as u32 => out.push_str("'$'\\x1e''"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn plain_words_are_wrapped() {
        assert_eq!(quote("abc"), "'abc'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn single_quotes_are_closed_escaped_reopened() {
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn record_terminator_is_spliced_as_dollar_quote() {
        assert_eq!(quote("a\x1eb"), "'a'$'\\x1e''b'");
    }

    #[test]
    fn everything_else_is_literal() {
        assert_eq!(quote("$HOME `x` \\ \"q\" \n\x1f"), "'$HOME `x` \\ \"q\" \n\x1f'");
    }
}
