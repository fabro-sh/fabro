/// Strip JSONC comments and trailing commas, producing valid JSON.
pub(crate) fn strip_jsonc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        match bytes[i] {
            // String literal — copy verbatim (including any comment-like content)
            b'"' => {
                out.push('"');
                i += 1;
                while i < len {
                    match bytes[i] {
                        b'\\' => {
                            // Escaped character — copy both backslash and next char
                            out.push('\\');
                            i += 1;
                            if i < len {
                                out.push(bytes[i] as char);
                                i += 1;
                            }
                        }
                        b'"' => {
                            out.push('"');
                            i += 1;
                            break;
                        }
                        _ => {
                            out.push(bytes[i] as char);
                            i += 1;
                        }
                    }
                }
            }

            // Potential comment start
            b'/' if i + 1 < len => {
                match bytes[i + 1] {
                    // Line comment — skip until end of line
                    b'/' => {
                        i += 2;
                        while i < len && bytes[i] != b'\n' {
                            i += 1;
                        }
                    }
                    // Block comment — skip until */
                    b'*' => {
                        i += 2;
                        while i + 1 < len {
                            if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                        // Handle unterminated block comment at end of input
                        if i >= len {
                            break;
                        }
                    }
                    _ => {
                        out.push('/');
                        i += 1;
                    }
                }
            }

            // Comma — check if it's a trailing comma before } or ]
            b',' => {
                // Look ahead past whitespace for } or ]
                let mut j = i + 1;
                while j < len && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                // Also skip comments after the comma
                while j < len {
                    if j + 1 < len && bytes[j] == b'/' && bytes[j + 1] == b'/' {
                        j += 2;
                        while j < len && bytes[j] != b'\n' {
                            j += 1;
                        }
                        while j < len && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                    } else if j + 1 < len && bytes[j] == b'/' && bytes[j + 1] == b'*' {
                        j += 2;
                        while j + 1 < len {
                            if bytes[j] == b'*' && bytes[j + 1] == b'/' {
                                j += 2;
                                break;
                            }
                            j += 1;
                        }
                        while j < len && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                    } else {
                        break;
                    }
                }

                if j < len && (bytes[j] == b'}' || bytes[j] == b']') {
                    // Trailing comma — skip it
                    i += 1;
                } else {
                    out.push(',');
                    i += 1;
                }
            }

            _ => {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_valid_json() {
        let json = r#"{"key": "value"}"#;
        assert_eq!(strip_jsonc(json), json);
    }

    #[test]
    fn strip_line_comments() {
        let input = r#"{
  // this is a comment
  "key": "value"
}"#;
        // Leading whitespace on the comment line remains but that's valid JSON
        let expected = "{\n  \n  \"key\": \"value\"\n}";
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn strip_line_comment_at_end_of_line() {
        let input = r#"{"key": "value" // inline comment
}"#;
        // Space before the comment remains
        let expected = "{\"key\": \"value\" \n}";
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn strip_block_comments() {
        let input = r#"{"key": /* comment */ "value"}"#;
        let expected = r#"{"key":  "value"}"#;
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn strip_multiline_block_comment() {
        let input = r#"{
  /* this is
     a multi-line
     comment */
  "key": "value"
}"#;
        // Leading whitespace before block comment remains
        let expected = "{\n  \n  \"key\": \"value\"\n}";
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn strip_trailing_comma_before_brace() {
        let input = r#"{"a": 1, "b": 2,}"#;
        let expected = r#"{"a": 1, "b": 2}"#;
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn strip_trailing_comma_before_bracket() {
        let input = r"[1, 2, 3,]";
        let expected = r"[1, 2, 3]";
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn trailing_comma_with_whitespace() {
        let input = r#"{
  "a": 1,
  "b": 2,
}"#;
        let expected = r#"{
  "a": 1,
  "b": 2
}"#;
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn comments_inside_strings_preserved() {
        let input = r#"{"key": "value // not a comment"}"#;
        assert_eq!(strip_jsonc(input), input);
    }

    #[test]
    fn block_comment_inside_string_preserved() {
        let input = r#"{"key": "value /* not a comment */ still here"}"#;
        assert_eq!(strip_jsonc(input), input);
    }

    #[test]
    fn mixed_comments_and_trailing_commas() {
        let input = r#"{
  // first comment
  "name": "test", /* inline */
  "items": [
    1,
    2, // trailing
  ],
}"#;
        // Whitespace around stripped comments remains; trailing commas removed
        let expected = "{\n  \n  \"name\": \"test\", \n  \"items\": [\n    1,\n    2 \n  ]\n}";
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn empty_input() {
        assert_eq!(strip_jsonc(""), "");
    }

    #[test]
    fn escaped_quote_in_string() {
        let input = r#"{"key": "val\"ue // not a comment"}"#;
        assert_eq!(strip_jsonc(input), input);
    }

    #[test]
    fn trailing_comma_with_comment_before_close() {
        let input = r#"{"a": 1, // comment
}"#;
        // Trailing comma removed; space before comment remains
        let expected = "{\"a\": 1 \n}";
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn trailing_comma_with_block_comment_before_close() {
        let input = r#"{"a": 1, /* comment */  }"#;
        // Trailing comma removed; spaces around stripped comment remain
        let expected = "{\"a\": 1   }";
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn only_comments() {
        let input = "// just a comment\n/* block */";
        let expected = "\n";
        assert_eq!(strip_jsonc(input), expected);
    }

    #[test]
    fn produces_valid_json() {
        let input = r#"{
  // devcontainer settings
  "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
  "features": {
    "ghcr.io/devcontainers/features/rust:1": {},
  },
  /* forwarded ports */
  "forwardPorts": [3000, 8080,],
  "remoteEnv": {
    "EDITOR": "code", // default editor
  },
}"#;
        let result = strip_jsonc(input);
        let parsed: serde_json::Result<serde_json::Value> = serde_json::from_str(&result);
        assert!(parsed.is_ok(), "should produce valid JSON, got: {result}");
    }
}
