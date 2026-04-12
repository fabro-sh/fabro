use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

static NUMERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+(\.\d+)*$").expect("valid regex"));

/// Sanitize CLI arguments for telemetry, redacting potentially sensitive
/// values.
///
/// Program path is reduced to its basename. Subcommand tokens are kept as-is.
/// Flags (`-*`) are kept, boolean literals and numeric patterns are kept,
/// `key=VALUE` splits and redacts the value side recursively, and everything
/// else is replaced with `"VALUE"`.
pub fn sanitize_command(args: &[String], subcommand: &str) -> String {
    let mut parts: Vec<String> = Vec::new();

    // argv[0]: basename only
    if let Some(program) = args.first() {
        let basename = Path::new(program)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(program);
        parts.push(basename.to_string());
    }

    // Determine how many tokens the subcommand consumes
    let sub_tokens: Vec<&str> = subcommand.split_whitespace().collect();
    let skip = 1 + sub_tokens.len(); // argv[0] + subcommand tokens

    // Add subcommand tokens verbatim
    parts.extend(sub_tokens.iter().map(std::string::ToString::to_string));

    // Sanitize remaining args
    for arg in args.iter().skip(skip) {
        parts.push(sanitize_arg(arg));
    }

    parts.join(" ")
}

fn sanitize_arg(arg: &str) -> String {
    if arg.starts_with('-') {
        // Flag with embedded value: --key=value
        if let Some(eq_pos) = arg.find('=') {
            let (key, val_with_eq) = arg.split_at(eq_pos);
            let val = &val_with_eq[1..]; // skip '='
            format!("{key}={}", sanitize_value(val))
        } else {
            arg.to_string()
        }
    } else if let Some(eq_pos) = arg.find('=') {
        let (key, val_with_eq) = arg.split_at(eq_pos);
        let val = &val_with_eq[1..];
        format!("{key}={}", sanitize_value(val))
    } else {
        sanitize_value(arg)
    }
}

fn sanitize_value(val: &str) -> String {
    let lower = val.to_ascii_lowercase();
    if matches!(lower.as_str(), "true" | "false" | "yes" | "no") {
        return val.to_string();
    }
    if NUMERIC_RE.is_match(val) {
        return val.to_string();
    }
    "VALUE".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn simple_command_with_flag() {
        let result = sanitize_command(
            &args(&["fabro", "run", "my-workflow.toml", "--no-retro"]),
            "run",
        );
        insta::assert_snapshot!(result, @"fabro run VALUE --no-retro");
    }

    #[test]
    fn nested_subcommand() {
        let result = sanitize_command(
            &args(&["/usr/local/bin/fabro", "pr", "create", "--title", "Fix bug"]),
            "pr create",
        );
        insta::assert_snapshot!(result, @"fabro pr create --title VALUE");
    }

    #[test]
    fn model_list_no_extra_args() {
        let result = sanitize_command(&args(&["fabro", "model", "list"]), "model list");
        insta::assert_snapshot!(result, @"fabro model list");
    }

    #[test]
    fn flag_with_equals_value() {
        let result = sanitize_command(&args(&["fabro", "run", "--level=medium"]), "run");
        insta::assert_snapshot!(result, @"fabro run --level=VALUE");
    }

    #[test]
    fn boolean_values_kept() {
        let result = sanitize_command(&args(&["fabro", "run", "--verbose", "true"]), "run");
        insta::assert_snapshot!(result, @"fabro run --verbose true");
    }

    #[test]
    fn file_paths_redacted() {
        let result = sanitize_command(&args(&["fabro", "check", "src/main.rs"]), "check");
        insta::assert_snapshot!(result, @"fabro check VALUE");
    }

    #[test]
    fn numeric_values_kept() {
        let result = sanitize_command(&args(&["fabro", "run", "--retries", "3"]), "run");
        insta::assert_snapshot!(result, @"fabro run --retries 3");
    }

    #[test]
    fn semver_kept() {
        let result = sanitize_command(&args(&["fabro", "run", "--min-version", "1.2.3"]), "run");
        insta::assert_snapshot!(result, @"fabro run --min-version 1.2.3");
    }

    #[test]
    fn positional_key_equals_value() {
        let result = sanitize_command(&args(&["fabro", "run", "env=production"]), "run");
        insta::assert_snapshot!(result, @"fabro run env=VALUE");
    }
}
