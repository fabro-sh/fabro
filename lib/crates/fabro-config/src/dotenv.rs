//! Shared utilities for reading and writing `.env` files (`~/.fabro/.env`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Return the path to `~/.fabro/.env`.
pub fn env_file_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".fabro").join(".env"))
}

/// Parse an env file's contents into `(key, value)` pairs, skipping comments
/// and blank lines. Values are split on the first `=` only.
pub fn parse_env(contents: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let value = trimmed[eq_pos + 1..].to_string();
            if !key.is_empty() {
                pairs.push((key, value));
            }
        }
    }
    pairs
}

/// Merge `new_vars` into an existing env file's contents, preserving comments,
/// blank lines, and ordering. Existing keys are updated in place; new keys are
/// appended. The returned string always ends with a newline.
pub fn merge_env(existing: &str, new_vars: &[(&str, &str)]) -> String {
    let mut result_lines: Vec<String> = Vec::new();
    let mut handled_keys: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for line in existing.lines() {
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            if !key.is_empty() && !key.starts_with('#') {
                if let Some((_, new_val)) = new_vars.iter().find(|(k, _)| *k == key) {
                    result_lines.push(format!("{key}={new_val}"));
                    handled_keys.insert(key);
                    continue;
                }
            }
        }
        result_lines.push(line.to_string());
    }

    for (key, val) in new_vars {
        if !handled_keys.contains(*key) {
            result_lines.push(format!("{key}={val}"));
        }
    }

    let mut result = result_lines.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Remove a key from an env file's raw contents. Comments and blank lines are
/// preserved. Returns `Some(new_contents)` if the key was found and removed,
/// or `None` if the key was not present.
pub fn remove_env_key(contents: &str, key_to_remove: &str) -> Option<String> {
    let mut result_lines: Vec<String> = Vec::new();
    let mut found = false;
    for line in contents.lines() {
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            if !key.is_empty() && !key.starts_with('#') && key == key_to_remove {
                found = true;
                continue;
            }
        }
        result_lines.push(line.to_string());
    }
    if !found {
        return None;
    }
    let mut result = result_lines.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    Some(result)
}

/// Write content to the env file, creating parent directories if needed.
/// Sets file permissions to 0600 on Unix.
pub fn write_env_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Read the value of a single key from the env file. Returns `None` if the
/// file doesn't exist or the key isn't found.
pub fn get_env_value(path: &Path, key: &str) -> Result<Option<String>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!("failed to read {}: {e}", path.display()),
    };
    let pairs = parse_env(&contents);
    Ok(pairs.into_iter().find(|(k, _)| k == key).map(|(_, v)| v))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_env --

    #[test]
    fn parse_env_basic() {
        let pairs = parse_env("FOO=bar\nBAZ=qux\n");
        assert_eq!(
            pairs,
            vec![("FOO".into(), "bar".into()), ("BAZ".into(), "qux".into())]
        );
    }

    #[test]
    fn parse_env_skips_comments_and_blanks() {
        let pairs = parse_env("# comment\n\nFOO=bar\n\n# another\n");
        assert_eq!(pairs, vec![("FOO".into(), "bar".into())]);
    }

    #[test]
    fn parse_env_values_with_equals() {
        let pairs = parse_env("URL=https://example.com?a=1&b=2\n");
        assert_eq!(
            pairs,
            vec![("URL".into(), "https://example.com?a=1&b=2".into())]
        );
    }

    // -- merge_env --

    #[test]
    fn merge_env_replaces_existing() {
        let result = merge_env("FOO=old\nBAR=keep\n", &[("FOO", "new"), ("BAZ", "added")]);
        assert!(result.contains("FOO=new"));
        assert!(result.contains("BAR=keep"));
        assert!(result.contains("BAZ=added"));
    }

    #[test]
    fn merge_env_empty_existing() {
        let result = merge_env("", &[("FOO", "bar"), ("BAZ", "qux")]);
        assert!(result.contains("FOO=bar"));
        assert!(result.contains("BAZ=qux"));
    }

    #[test]
    fn merge_env_preserves_comments_and_blanks() {
        let existing = "# A comment\n\nFOO=old\n# Another\nBAR=keep\n";
        let result = merge_env(existing, &[("FOO", "new")]);
        assert!(result.contains("# A comment"));
        assert!(result.contains("# Another"));
        assert!(result.contains("FOO=new"));
        assert!(result.contains("BAR=keep"));
    }

    #[test]
    fn merge_env_full_scenario() {
        let result = merge_env("FOO=old\nBAR=keep", &[("FOO", "new"), ("BAZ", "added")]);
        assert_eq!(result, "FOO=new\nBAR=keep\nBAZ=added\n");
    }

    // -- remove_env_key --

    #[test]
    fn remove_env_key_removes_target() {
        let result = remove_env_key("FOO=bar\nBAZ=qux\n", "FOO").unwrap();
        assert!(!result.contains("FOO"));
        assert!(result.contains("BAZ=qux"));
    }

    #[test]
    fn remove_env_key_preserves_others() {
        let result = remove_env_key("A=1\nB=2\nC=3\n", "B").unwrap();
        assert!(result.contains("A=1"));
        assert!(!result.contains("B=2"));
        assert!(result.contains("C=3"));
    }

    #[test]
    fn remove_env_key_returns_none_for_missing_key() {
        assert!(remove_env_key("FOO=bar\n", "MISSING").is_none());
    }

    #[test]
    fn remove_env_key_preserves_comments() {
        let result = remove_env_key("# keep me\nFOO=bar\nBAZ=qux\n", "FOO").unwrap();
        assert!(result.contains("# keep me"));
        assert!(result.contains("BAZ=qux"));
        assert!(!result.contains("FOO"));
    }

    // -- write_env_file --

    #[test]
    fn write_env_file_creates_and_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub").join(".env");
        write_env_file(&path, "KEY=val\n").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "KEY=val\n");
    }

    // -- get_env_value --

    #[test]
    fn get_env_value_found() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".env");
        std::fs::write(&path, "FOO=bar\nBAZ=qux\n").unwrap();
        assert_eq!(get_env_value(&path, "FOO").unwrap(), Some("bar".into()));
        assert_eq!(get_env_value(&path, "BAZ").unwrap(), Some("qux".into()));
    }

    #[test]
    fn get_env_value_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".env");
        std::fs::write(&path, "FOO=bar\n").unwrap();
        assert_eq!(get_env_value(&path, "MISSING").unwrap(), None);
    }

    #[test]
    fn get_env_value_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent");
        assert_eq!(get_env_value(&path, "FOO").unwrap(), None);
    }
}
