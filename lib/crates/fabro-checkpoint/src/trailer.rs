use std::fmt::Write;

/// A git commit message trailer (key-value pair).
pub struct Trailer<'a> {
    pub key: &'a str,
    pub value: &'a str,
}

/// Append a trailer to a commit message, inserting a blank-line separator if needed.
pub fn append(message: &str, trailer: &Trailer<'_>) -> String {
    let trailer_line = format!("{}: {}", trailer.key, trailer.value);
    let trimmed = message.trim_end();

    if trimmed.is_empty() {
        return trailer_line;
    }

    // Check if the message already ends with a trailer block.
    // A trailer block is preceded by a blank line and every line contains ": ".
    if has_trailing_trailer_block(trimmed) {
        format!("{trimmed}\n{trailer_line}\n")
    } else {
        format!("{trimmed}\n\n{trailer_line}\n")
    }
}

/// Extract a trailer value from a commit message. Scans from the end.
pub fn parse<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}: ");
    // Scan lines from end to find the trailer
    for line in message.lines().rev() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(&prefix) {
            return Some(value);
        }
        // Stop scanning if we hit a blank line (left the trailer block)
        if trimmed.is_empty() {
            break;
        }
    }
    None
}

/// Build a commit message from subject, body, and trailers.
pub fn format_message(subject: &str, body: &str, trailers: &[Trailer<'_>]) -> String {
    let mut msg = subject.to_string();

    if !body.is_empty() {
        msg.push_str("\n\n");
        msg.push_str(body);
    }

    if !trailers.is_empty() {
        msg.push_str("\n\n");
        for trailer in trailers {
            let _ = writeln!(msg, "{}: {}", trailer.key, trailer.value);
        }
    }

    if !msg.ends_with('\n') {
        msg.push('\n');
    }

    msg
}

/// Check if the trimmed message ends with a trailer block.
/// A trailer block: every line after the last blank line contains ": ".
fn has_trailing_trailer_block(trimmed: &str) -> bool {
    let mut found_blank = false;
    let mut trailer_lines = 0;

    for line in trimmed.lines().rev() {
        if line.trim().is_empty() {
            found_blank = true;
            break;
        }
        if line.contains(": ") {
            trailer_lines += 1;
        } else {
            return false;
        }
    }

    found_blank && trailer_lines > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_to_simple_message() {
        let result = append(
            "Initial commit",
            &Trailer {
                key: "My-Checkpoint",
                value: "abc123",
            },
        );
        assert_eq!(result, "Initial commit\n\nMy-Checkpoint: abc123\n");
    }

    #[test]
    fn append_to_message_with_existing_trailer() {
        let msg = "Initial commit\n\nSigned-off-by: Alice <alice@example.com>\n";
        let result = append(
            msg,
            &Trailer {
                key: "My-Checkpoint",
                value: "abc123",
            },
        );
        assert_eq!(
            result,
            "Initial commit\n\nSigned-off-by: Alice <alice@example.com>\nMy-Checkpoint: abc123\n"
        );
    }

    #[test]
    fn append_to_message_with_body_no_trailer() {
        let msg = "Initial commit\n\nThis is a longer description of the change.\n";
        let result = append(
            msg,
            &Trailer {
                key: "My-Checkpoint",
                value: "abc123",
            },
        );
        assert_eq!(
            result,
            "Initial commit\n\nThis is a longer description of the change.\n\nMy-Checkpoint: abc123\n"
        );
    }

    #[test]
    fn parse_finds_trailer() {
        let msg = "Initial commit\n\nMy-Checkpoint: abc123\n";
        assert_eq!(parse(msg, "My-Checkpoint"), Some("abc123"));
    }

    #[test]
    fn parse_finds_trailer_among_multiple() {
        let msg =
            "Initial commit\n\nSigned-off-by: Alice <alice@example.com>\nMy-Checkpoint: abc123\n";
        assert_eq!(parse(msg, "My-Checkpoint"), Some("abc123"));
        assert_eq!(
            parse(msg, "Signed-off-by"),
            Some("Alice <alice@example.com>")
        );
    }

    #[test]
    fn parse_returns_none_when_missing() {
        let msg = "Initial commit\n\nSigned-off-by: Alice\n";
        assert_eq!(parse(msg, "My-Checkpoint"), None);
    }

    #[test]
    fn parse_returns_none_on_empty_message() {
        assert_eq!(parse("", "My-Checkpoint"), None);
    }

    #[test]
    fn format_message_subject_only() {
        let result = format_message("Initial commit", "", &[]);
        assert_eq!(result, "Initial commit\n");
    }

    #[test]
    fn format_message_with_body() {
        let result = format_message("Initial commit", "Detailed description", &[]);
        assert_eq!(result, "Initial commit\n\nDetailed description\n");
    }

    #[test]
    fn format_message_with_trailers() {
        let result = format_message(
            "Initial commit",
            "",
            &[
                Trailer {
                    key: "Signed-off-by",
                    value: "Alice",
                },
                Trailer {
                    key: "My-Checkpoint",
                    value: "abc123",
                },
            ],
        );
        assert_eq!(
            result,
            "Initial commit\n\nSigned-off-by: Alice\nMy-Checkpoint: abc123\n"
        );
    }

    #[test]
    fn format_message_with_body_and_trailers() {
        let result = format_message(
            "Initial commit",
            "Description here",
            &[Trailer {
                key: "My-Checkpoint",
                value: "abc123",
            }],
        );
        assert_eq!(
            result,
            "Initial commit\n\nDescription here\n\nMy-Checkpoint: abc123\n"
        );
    }
}
