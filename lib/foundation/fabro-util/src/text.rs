use console::strip_ansi_codes;

/// Longest display label kept from runtime data before it is elided.
const MAX_DISPLAY_LABEL: usize = 80;

/// Characters that reorder rendered text without being control characters:
/// the bidi embedding, override, and isolate marks.
fn is_bidi_control(ch: char) -> bool {
    matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
}

/// Make a string that came from a model or a user safe to print in a terminal.
///
/// Strips ANSI escape sequences, then removes control and bidi-reordering
/// characters, trims surrounding whitespace, and elides anything past
/// `MAX_DISPLAY_LABEL`. Any terminal-facing identifier built from runtime
/// data should go through this — without it a label can move the cursor,
/// inject color, or reverse the text around it.
///
/// Returns an empty string when nothing printable survives, so callers can
/// fall back to an identity they control.
#[must_use]
pub fn sanitize_display_label(label: &str) -> String {
    let stripped = strip_ansi_codes(label);
    let cleaned = stripped
        .chars()
        .filter(|ch| !ch.is_control() && !is_bidi_control(*ch))
        .collect::<String>();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() > MAX_DISPLAY_LABEL {
        trimmed
            .chars()
            .take(MAX_DISPLAY_LABEL)
            .chain(std::iter::once('…'))
            .collect()
    } else {
        trimmed.to_string()
    }
}

/// Strip markdown heading prefixes and `Plan:` prefix from a goal string.
///
/// Takes the first line, removes all leading `#` characters, then strips
/// a `Plan:` prefix if present. Returns a trimmed `&str` slice.
pub fn strip_goal_decoration(goal: &str) -> &str {
    let line = goal.lines().next().unwrap_or("");
    let line = line.trim_start_matches('#').trim();
    line.strip_prefix("Plan:").map_or(line, str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_display_label_removes_ansi_and_control_characters() {
        assert_eq!(sanitize_display_label("\u{1b}[31mauth\u{1b}[0m"), "auth");
        assert_eq!(sanitize_display_label("auth\nreviewer"), "authreviewer");
        assert_eq!(sanitize_display_label("auth\r\u{7}"), "auth");
        assert_eq!(sanitize_display_label("  auth  "), "auth");
    }

    #[test]
    fn sanitize_display_label_removes_bidi_reordering_marks() {
        assert_eq!(sanitize_display_label("auth\u{202e}resu"), "authresu");
        assert_eq!(sanitize_display_label("\u{2066}auth\u{2069}"), "auth");
    }

    #[test]
    fn sanitize_display_label_elides_long_labels() {
        let sanitized = sanitize_display_label(&"a".repeat(200));
        assert_eq!(sanitized.chars().count(), MAX_DISPLAY_LABEL + 1);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn sanitize_display_label_is_empty_when_nothing_printable_remains() {
        assert_eq!(sanitize_display_label("   "), "");
        assert_eq!(sanitize_display_label("\u{1b}[0m\n\t"), "");
    }

    #[test]
    fn strips_h1() {
        assert_eq!(strip_goal_decoration("# Title"), "Title");
    }

    #[test]
    fn strips_h2() {
        assert_eq!(strip_goal_decoration("## Fix bug"), "Fix bug");
    }

    #[test]
    fn strips_h3() {
        assert_eq!(strip_goal_decoration("### Deep heading"), "Deep heading");
    }

    #[test]
    fn strips_plan_prefix() {
        assert_eq!(strip_goal_decoration("Plan: do stuff"), "do stuff");
    }

    #[test]
    fn strips_heading_and_plan_prefix() {
        assert_eq!(strip_goal_decoration("## Plan: migrate DB"), "migrate DB");
    }

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(
            strip_goal_decoration("Fix the login bug"),
            "Fix the login bug"
        );
    }

    #[test]
    fn takes_first_line() {
        assert_eq!(
            strip_goal_decoration("## Plan: First\n\nMore details"),
            "First"
        );
    }

    #[test]
    fn empty_string() {
        assert_eq!(strip_goal_decoration(""), "");
    }
}
