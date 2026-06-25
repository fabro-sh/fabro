//! Shell-quoting helpers.
//!
//! A single audited place that turns an arbitrary string into a token that is
//! safe to interpolate into a `/bin/sh` command line. Use this anywhere a
//! user-controlled value (path, branch name, URL, env var, glob, argv element)
//! is assembled into a shell script — never hand-roll the escaping.

/// Shell-quote a string using `shlex::try_quote`, with a fallback for edge
/// cases (`shlex` rejects strings containing a NUL byte, which can never appear
/// in a real argv anyway, so the fallback simply single-quotes defensively).
#[must_use]
pub fn shell_quote(s: &str) -> String {
    shlex::try_quote(s).map_or_else(
        |_| format!("'{}'", s.replace('\'', "'\\''")),
        std::borrow::Cow::into_owned,
    )
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn plain_word_is_unquoted() {
        assert_eq!(shell_quote("setup"), "setup");
    }

    #[test]
    fn spaces_are_quoted() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn single_quotes_are_escaped() {
        // shlex double-quotes a token that contains a single quote.
        assert_eq!(shell_quote("it's"), r#""it's""#);
    }
}
