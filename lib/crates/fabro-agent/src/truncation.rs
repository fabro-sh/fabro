use crate::config::SessionConfig;

/// Round a byte index down to the nearest UTF-8 char boundary.
/// Stable equivalent of `str::floor_char_boundary` (nightly-only).
#[must_use]
pub fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationMode {
    HeadTail,
    Tail,
}

fn default_char_limit(tool_name: &str) -> Option<usize> {
    match tool_name {
        "read_file" => Some(50_000),
        "shell" => Some(30_000),
        "grep" | "glob" | "spawn_agent" => Some(20_000),
        "edit_file" | "apply_patch" => Some(10_000),
        "write_file" => Some(1_000),
        _ => None,
    }
}

fn default_line_limit(tool_name: &str) -> Option<usize> {
    match tool_name {
        "shell" => Some(256),
        "grep" => Some(200),
        "glob" => Some(500),
        _ => None,
    }
}

fn default_truncation_mode(tool_name: &str) -> TruncationMode {
    match tool_name {
        "grep" | "glob" | "edit_file" | "apply_patch" | "write_file" => TruncationMode::Tail,
        _ => TruncationMode::HeadTail,
    }
}

#[must_use]
pub fn truncate_output(output: &str, max_chars: usize, mode: TruncationMode) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }

    let removed = output.len() - max_chars;

    match mode {
        TruncationMode::HeadTail => {
            let half = max_chars / 2;
            let head_end = floor_char_boundary(output, half);
            let tail_start = floor_char_boundary(output, output.len() - half);
            let head = &output[..head_end];
            let tail = &output[tail_start..];
            format!(
                "{head}\n\n[WARNING: Tool output was truncated. {removed} characters were removed from the middle. \
                 The full output is available in the event stream. \
                 If you need to see specific parts, re-run the tool with more targeted parameters.]\n\n{tail}"
            )
        }
        TruncationMode::Tail => {
            let tail_start = floor_char_boundary(output, output.len() - max_chars);
            let tail = &output[tail_start..];
            format!(
                "[WARNING: Tool output was truncated. First {removed} characters were removed. \
                 The full output is available in the event stream.]\n\n{tail}"
            )
        }
    }
}

#[must_use]
pub fn truncate_lines(output: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= max_lines {
        return output.to_string();
    }

    let half = max_lines / 2;
    let head: Vec<&str> = lines[..half].to_vec();
    let tail: Vec<&str> = lines[lines.len() - half..].to_vec();
    let omitted = lines.len() - max_lines;

    format!(
        "{}\n\n[... {omitted} lines omitted ...]\n\n{}",
        head.join("\n"),
        tail.join("\n")
    )
}

#[must_use]
pub fn truncate_tool_output(output: &str, tool_name: &str, config: &SessionConfig) -> String {
    let mode = default_truncation_mode(tool_name);

    // Char truncation first
    let char_limit = config
        .tool_output_limits
        .get(tool_name)
        .copied()
        .or_else(|| default_char_limit(tool_name));

    let after_chars = match char_limit {
        Some(limit) => truncate_output(output, limit, mode),
        None => output.to_string(),
    };

    // Then line truncation
    let line_limit = config
        .tool_line_limits
        .get(tool_name)
        .copied()
        .or_else(|| default_line_limit(tool_name));

    match line_limit {
        Some(limit) => truncate_lines(&after_chars, limit),
        None => after_chars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_limit_passthrough_chars() {
        let output = "short output";
        let result = truncate_output(output, 100, TruncationMode::HeadTail);
        assert_eq!(result, output);
    }

    #[test]
    fn under_limit_passthrough_lines() {
        let output = "line1\nline2\nline3";
        let result = truncate_lines(output, 10);
        assert_eq!(result, output);
    }

    #[test]
    fn head_tail_split() {
        let output = "a".repeat(100);
        let result = truncate_output(&output, 40, TruncationMode::HeadTail);
        assert!(result.contains(&"a".repeat(20)));
        assert!(result.contains("Tool output was truncated"));
        assert!(result.contains("60 characters were removed from the middle"));
    }

    #[test]
    fn tail_mode() {
        let output = format!("{}BBB", "A".repeat(100));
        let result = truncate_output(&output, 10, TruncationMode::Tail);
        assert!(result.contains("Tool output was truncated"));
        assert!(result.contains("First 93 characters were removed"));
        assert!(result.ends_with("AAAAAAABBB"));
    }

    #[test]
    fn line_truncation_splits_head_tail() {
        let lines: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
        let output = lines.join("\n");
        let result = truncate_lines(&output, 6);
        assert!(result.contains("line 1"));
        assert!(result.contains("line 3"));
        assert!(result.contains("line 18"));
        assert!(result.contains("line 20"));
        assert!(result.contains("... 14 lines omitted ..."));
    }

    #[test]
    fn char_truncation_before_lines() {
        // Create an output that is large in chars and many lines
        let long_line = "x".repeat(50_000);
        let output = format!("{long_line}\n{long_line}");
        let config = SessionConfig::default();
        let result = truncate_tool_output(&output, "shell", &config);
        // Should have been char-truncated first (30k limit for shell)
        assert!(result.len() < output.len());
    }

    #[test]
    fn config_override_char_limit() {
        let output = "x".repeat(5000);
        let mut config = SessionConfig::default();
        config.tool_output_limits.insert("my_tool".into(), 100);
        let result = truncate_tool_output(&output, "my_tool", &config);
        assert!(result.len() < output.len());
        assert!(result.contains("Tool output was truncated"));
    }

    #[test]
    fn config_override_line_limit() {
        let lines: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        let output = lines.join("\n");
        let mut config = SessionConfig::default();
        config.tool_line_limits.insert("my_tool".into(), 10);
        let result = truncate_tool_output(&output, "my_tool", &config);
        assert!(result.contains("lines omitted"));
    }

    #[test]
    fn unknown_tool_no_truncation() {
        let output = "x".repeat(200);
        let config = SessionConfig::default();
        let result = truncate_tool_output(&output, "unknown_tool", &config);
        assert_eq!(result, output);
    }

    #[test]
    fn default_char_limits_match_spec() {
        assert_eq!(default_char_limit("read_file"), Some(50_000));
        assert_eq!(default_char_limit("shell"), Some(30_000));
        assert_eq!(default_char_limit("grep"), Some(20_000));
        assert_eq!(default_char_limit("glob"), Some(20_000));
        assert_eq!(default_char_limit("edit_file"), Some(10_000));
        assert_eq!(default_char_limit("write_file"), Some(1_000));
        assert_eq!(default_char_limit("apply_patch"), Some(10_000));
        assert_eq!(default_char_limit("spawn_agent"), Some(20_000));
        assert_eq!(default_char_limit("unknown"), None);
    }

    #[test]
    fn default_line_limits_match_spec() {
        assert_eq!(default_line_limit("shell"), Some(256));
        assert_eq!(default_line_limit("grep"), Some(200));
        assert_eq!(default_line_limit("glob"), Some(500));
        assert_eq!(default_line_limit("unknown"), None);
    }

    #[test]
    fn exact_limit_not_truncated() {
        let output = "x".repeat(100);
        let result = truncate_output(&output, 100, TruncationMode::HeadTail);
        assert_eq!(result, output);
    }

    #[test]
    fn exact_line_limit_not_truncated() {
        let lines: Vec<String> = (1..=10).map(|i| format!("line {i}")).collect();
        let output = lines.join("\n");
        let result = truncate_lines(&output, 10);
        assert_eq!(result, output);
    }

    #[test]
    fn floor_char_boundary_ascii() {
        assert_eq!(floor_char_boundary("hello", 3), 3);
        assert_eq!(floor_char_boundary("hello", 10), 5);
        assert_eq!(floor_char_boundary("hello", 0), 0);
    }

    #[test]
    fn floor_char_boundary_multibyte() {
        // ✅ is 3 bytes (E2 9C 85)
        let s = "a✅b";
        assert_eq!(floor_char_boundary(s, 1), 1); // just past 'a'
        assert_eq!(floor_char_boundary(s, 2), 1); // inside ✅, rounds down to 'a'
        assert_eq!(floor_char_boundary(s, 3), 1); // still inside ✅
        assert_eq!(floor_char_boundary(s, 4), 4); // at 'b'
    }

    #[test]
    fn truncate_output_multibyte_no_panic() {
        let output = "✅".repeat(100); // 300 bytes
        let result = truncate_output(&output, 10, TruncationMode::HeadTail);
        assert!(result.contains("Tool output was truncated"));
    }
}
