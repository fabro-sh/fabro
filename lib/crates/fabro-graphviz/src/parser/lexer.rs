/// Strip `//` line comments and `/* */` block comments from DOT source.
#[must_use]
pub fn strip_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if i + 1 < len && chars[i] == '/' && chars[i + 1] == '/' {
            // Line comment: skip to end of line
            i += 2;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
        } else if i + 1 < len && chars[i] == '/' && chars[i + 1] == '*' {
            // Block comment: skip to closing */
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    result.push('\n');
                }
                i += 1;
            }
            if i + 1 < len {
                i += 2; // skip */
            }
        } else if chars[i] == '"' {
            // Quoted string: pass through without stripping
            result.push(chars[i]);
            i += 1;
            while i < len && chars[i] != '"' {
                result.push(chars[i]);
                if chars[i] == '\\' && i + 1 < len {
                    i += 1;
                    result.push(chars[i]);
                }
                i += 1;
            }
            if i < len {
                result.push(chars[i]); // closing quote
                i += 1;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// nom combinators for whitespace and common tokens.
pub mod combinators {
    use nom::branch::alt;
    use nom::bytes::complete::{tag, take_while, take_while1};
    use nom::character::complete::{char, multispace0};
    use nom::combinator::{map, opt, recognize};
    use nom::error::{Error, ErrorKind};
    use nom::sequence::{delimited, pair, preceded};
    use nom::{Err, IResult};

    use crate::parser::ast::AstValue;

    /// Parse optional whitespace (including newlines).
    pub fn ws(input: &str) -> IResult<&str, &str> {
        multispace0(input)
    }

    /// Parse a token surrounded by optional whitespace.
    pub fn ws_tag<'a>(t: &'a str) -> impl Fn(&'a str) -> IResult<&'a str, &'a str> {
        move |input| delimited(ws, tag(t), ws)(input)
    }

    /// Parse an identifier: `[A-Za-z_][A-Za-z0-9_]*`.
    pub fn identifier(input: &str) -> IResult<&str, &str> {
        recognize(pair(
            take_while1(|c: char| c.is_ascii_alphabetic() || c == '_'),
            take_while(|c: char| c.is_ascii_alphanumeric() || c == '_'),
        ))(input)
    }

    /// Parse a qualified ID: `identifier(.identifier)+`.
    pub fn qualified_id(input: &str) -> IResult<&str, String> {
        let (rest, first) = identifier(input)?;
        let mut result = first.to_string();
        let mut remaining = rest;
        let mut found_dot = false;
        while let Ok((r, _)) = char::<&str, Error<&str>>('.')(remaining) {
            if let Ok((r2, segment)) = identifier(r) {
                result.push('.');
                result.push_str(segment);
                remaining = r2;
                found_dot = true;
            } else {
                break;
            }
        }
        if found_dot {
            Ok((remaining, result))
        } else {
            Err(Err::Error(Error::new(input, ErrorKind::Tag)))
        }
    }

    /// Parse a key: either a qualified ID or a simple identifier.
    pub fn key(input: &str) -> IResult<&str, String> {
        alt((qualified_id, map(identifier, String::from)))(input)
    }

    /// Parse a double-quoted string with escape handling.
    pub fn quoted_string(input: &str) -> IResult<&str, String> {
        let (input, _) = char('"')(input)?;
        let mut result = String::new();
        let mut chars = input.chars();
        let mut consumed = 0;

        loop {
            match chars.next() {
                Some('"') => {
                    consumed += 1;
                    return Ok((&input[consumed..], result));
                }
                Some('\\') => {
                    consumed += 1;
                    match chars.next() {
                        Some('"') => {
                            result.push('"');
                            consumed += 1;
                        }
                        Some('n') => {
                            result.push('\n');
                            consumed += 1;
                        }
                        Some('t') => {
                            result.push('\t');
                            consumed += 1;
                        }
                        Some('\\') => {
                            result.push('\\');
                            consumed += 1;
                        }
                        Some(c) => {
                            result.push('\\');
                            result.push(c);
                            consumed += c.len_utf8();
                        }
                        None => {
                            return Err(Err::Error(Error::new(input, ErrorKind::Char)));
                        }
                    }
                }
                Some(c) => {
                    result.push(c);
                    consumed += c.len_utf8();
                }
                None => {
                    return Err(Err::Error(Error::new(input, ErrorKind::Char)));
                }
            }
        }
    }

    /// Parse a boolean: `true` or `false`.
    pub fn boolean(input: &str) -> IResult<&str, bool> {
        let (rest, word) = identifier(input)?;
        match word {
            "true" => Ok((rest, true)),
            "false" => Ok((rest, false)),
            _ => Err(Err::Error(Error::new(input, ErrorKind::Tag))),
        }
    }

    /// Parse a float: optional sign, optional integer part, `.`, fractional
    /// digits.
    pub fn float_value(input: &str) -> IResult<&str, f64> {
        let (rest, raw) = recognize(pair(
            pair(opt(char('-')), take_while(|c: char| c.is_ascii_digit())),
            pair(char('.'), take_while1(|c: char| c.is_ascii_digit())),
        ))(input)?;
        let val: f64 = raw
            .parse()
            .map_err(|_| Err::Error(Error::new(input, ErrorKind::Float)))?;
        Ok((rest, val))
    }

    /// Parse an integer: optional sign, digits. Not followed by `.` (that's a
    /// float).
    pub fn integer_value(input: &str) -> IResult<&str, i64> {
        let (rest, raw) = recognize(pair(
            opt(char('-')),
            take_while1(|c: char| c.is_ascii_digit()),
        ))(input)?;
        if rest.starts_with('.') {
            return Err(Err::Error(Error::new(input, ErrorKind::Digit)));
        }
        let val: i64 = raw
            .parse()
            .map_err(|_| Err::Error(Error::new(input, ErrorKind::Digit)))?;
        Ok((rest, val))
    }

    /// Parse a duration: integer followed by unit suffix (ms, s, m, h, d).
    pub fn duration_value(input: &str) -> IResult<&str, AstValue> {
        let (rest, num) = recognize(pair(
            opt(char('-')),
            take_while1(|c: char| c.is_ascii_digit()),
        ))(input)?;
        let (rest, unit) = alt((tag("ms"), tag("s"), tag("m"), tag("h"), tag("d")))(rest)?;
        if rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return Err(Err::Error(Error::new(input, ErrorKind::Tag)));
        }
        Ok((rest, AstValue::Str(format!("{num}{unit}"))))
    }

    /// Parse a bare string value containing hyphens and dots (e.g.,
    /// `gpt-5.2-codex`).
    ///
    /// Must start with an alpha/underscore character, then may continue with
    /// alphanumeric, underscore, hyphen, or dot characters. Must contain at
    /// least one hyphen or dot (otherwise `identifier` handles it).
    pub fn bare_string(input: &str) -> IResult<&str, String> {
        let (rest, raw) = recognize(pair(
            take_while1(|c: char| c.is_ascii_alphabetic() || c == '_'),
            take_while(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'),
        ))(input)?;
        if !raw.contains('-') && !raw.contains('.') {
            return Err(Err::Error(Error::new(input, ErrorKind::Verify)));
        }
        Ok((rest, raw.to_string()))
    }

    /// Parse an AST value: duration, float, integer, boolean, quoted string,
    /// bare identifier, or bare string (e.g., `gpt-5.2-codex`).
    pub fn value(input: &str) -> IResult<&str, AstValue> {
        let input = input.trim_start();
        alt((
            map(quoted_string, AstValue::Str),
            duration_value,
            map(float_value, AstValue::Float),
            map(integer_value, AstValue::Int),
            map(boolean, AstValue::Bool),
            map(bare_string, AstValue::Str),
            map(identifier, |s: &str| AstValue::Ident(s.to_string())),
        ))(input)
    }

    /// Parse the arrow operator `->` surrounded by optional whitespace.
    pub fn arrow(input: &str) -> IResult<&str, &str> {
        preceded(ws, tag("->"))(input)
    }
}

#[cfg(test)]
mod tests {
    use super::combinators::*;
    use super::*;
    use crate::parser::ast::AstValue;

    #[test]
    fn strip_line_comments() {
        let input = "hello // this is a comment\nworld";
        assert_eq!(strip_comments(input), "hello \nworld");
    }

    #[test]
    fn strip_block_comments() {
        let input = "before /* inside */ after";
        assert_eq!(strip_comments(input), "before  after");
    }

    #[test]
    fn strip_block_comments_multiline() {
        let input = "a /* line1\nline2 */ b";
        let result = strip_comments(input);
        assert_eq!(result, "a \n b");
    }

    #[test]
    fn strip_preserves_strings() {
        let input = r#""hello // not a comment" rest"#;
        assert_eq!(strip_comments(input), r#""hello // not a comment" rest"#);
    }

    #[test]
    fn strip_string_with_escapes() {
        let input = r#""escaped \" quote" rest"#;
        assert_eq!(strip_comments(input), r#""escaped \" quote" rest"#);
    }

    #[test]
    fn parse_identifier() {
        assert_eq!(identifier("hello_world123 "), Ok((" ", "hello_world123")));
        assert_eq!(identifier("_private rest"), Ok((" rest", "_private")));
        assert!(identifier("123abc").is_err());
    }

    #[test]
    fn parse_qualified_id() {
        assert_eq!(
            qualified_id("tool_hooks.pre rest"),
            Ok((" rest", "tool_hooks.pre".into()))
        );
        assert_eq!(qualified_id("a.b.c rest"), Ok((" rest", "a.b.c".into())));
        assert!(qualified_id("simple rest").is_err());
    }

    #[test]
    fn parse_key_simple_and_qualified() {
        assert_eq!(key("label rest"), Ok((" rest", "label".into())));
        assert_eq!(
            key("tool_hooks.pre rest"),
            Ok((" rest", "tool_hooks.pre".into()))
        );
    }

    #[test]
    fn parse_quoted_string() {
        assert_eq!(quoted_string(r#""hello""#), Ok(("", "hello".into())));
        assert_eq!(
            quoted_string(r#""line1\nline2""#),
            Ok(("", "line1\nline2".into()))
        );
        assert_eq!(
            quoted_string(r#""tab\there""#),
            Ok(("", "tab\there".into()))
        );
        assert_eq!(
            quoted_string(r#""escaped \" quote""#),
            Ok(("", "escaped \" quote".into()))
        );
        assert_eq!(
            quoted_string(r#""back\\slash""#),
            Ok(("", "back\\slash".into()))
        );
    }

    #[test]
    fn parse_boolean() {
        assert_eq!(boolean("true rest"), Ok((" rest", true)));
        assert_eq!(boolean("false rest"), Ok((" rest", false)));
        assert!(boolean("yes").is_err());
    }

    #[test]
    fn parse_integer() {
        assert_eq!(integer_value("42 rest"), Ok((" rest", 42)));
        assert_eq!(integer_value("-1 rest"), Ok((" rest", -1)));
        assert_eq!(integer_value("0 rest"), Ok((" rest", 0)));
        assert!(integer_value("42.5").is_err());
    }

    #[test]
    fn parse_float() {
        assert_eq!(float_value("3.15 rest"), Ok((" rest", 3.15)));
        assert_eq!(float_value("0.5 rest"), Ok((" rest", 0.5)));
        assert_eq!(float_value("-3.15 rest"), Ok((" rest", -3.15)));
        assert_eq!(float_value(".5 rest"), Ok((" rest", 0.5)));
    }

    #[test]
    fn parse_duration() {
        assert_eq!(
            duration_value("250ms rest"),
            Ok((" rest", AstValue::Str("250ms".into())))
        );
        assert_eq!(
            duration_value("900s rest"),
            Ok((" rest", AstValue::Str("900s".into())))
        );
        assert_eq!(
            duration_value("15m rest"),
            Ok((" rest", AstValue::Str("15m".into())))
        );
        assert_eq!(
            duration_value("2h rest"),
            Ok((" rest", AstValue::Str("2h".into())))
        );
        assert_eq!(
            duration_value("1d rest"),
            Ok((" rest", AstValue::Str("1d".into())))
        );
    }

    #[test]
    fn parse_value_all_types() {
        assert_eq!(value(r#""hello""#), Ok(("", AstValue::Str("hello".into()))));
        assert_eq!(value("250ms"), Ok(("", AstValue::Str("250ms".into()))));
        assert_eq!(value("3.15"), Ok(("", AstValue::Float(3.15))));
        assert_eq!(value("42"), Ok(("", AstValue::Int(42))));
        assert_eq!(value("true"), Ok(("", AstValue::Bool(true))));
        assert_eq!(value("LR"), Ok(("", AstValue::Ident("LR".into()))));
    }

    #[test]
    fn parse_bare_string_with_hyphens_and_dots() {
        assert_eq!(bare_string("gpt-5.2 rest"), Ok((" rest", "gpt-5.2".into())));
        assert_eq!(
            bare_string("gpt-5.2-codex"),
            Ok(("", "gpt-5.2-codex".into()))
        );
        assert_eq!(
            bare_string("gpt-5.3-codex-spark"),
            Ok(("", "gpt-5.3-codex-spark".into()))
        );
        assert_eq!(
            bare_string("gemini-3-flash-preview"),
            Ok(("", "gemini-3-flash-preview".into()))
        );
        // Plain identifier without hyphens/dots should fail (identifier handles it)
        assert!(bare_string("LR").is_err());
        assert!(bare_string("openai").is_err());
    }

    #[test]
    fn parse_value_bare_string() {
        assert_eq!(value("gpt-5.2"), Ok(("", AstValue::Str("gpt-5.2".into()))));
        assert_eq!(
            value("gpt-5.2-codex"),
            Ok(("", AstValue::Str("gpt-5.2-codex".into())))
        );
    }
}
