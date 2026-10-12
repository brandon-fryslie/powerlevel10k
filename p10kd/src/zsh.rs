//! Rendering values as zsh source text.
//!
//! The daemon's replies are evaluated by the shell, so every fact crosses this one
//! boundary as a single-quoted zsh word. [LAW:single-enforcer] quoting lives here only.

/// Quote `s` as one zsh word that evaluates back to exactly `s`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
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
    fn everything_else_is_literal() {
        assert_eq!(quote("$HOME `x` \\ \"q\" \n"), "'$HOME `x` \\ \"q\" \n'");
    }
}
