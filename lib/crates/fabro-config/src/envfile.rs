#![expect(
    clippy::disallowed_methods,
    reason = "sync env-file load/save used at CLI and server startup; not on a Tokio path"
)]

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

pub fn read_env_file(path: &Path) -> io::Result<HashMap<String, String>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(err),
    };

    let mut entries = HashMap::new();
    for (index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            return Err(invalid_data(format!(
                "invalid env line {} in {}",
                index + 1,
                path.display()
            )));
        };

        let key = raw_key.trim();
        if key.is_empty() {
            return Err(invalid_data(format!(
                "empty env key on line {} in {}",
                index + 1,
                path.display()
            )));
        }

        entries.insert(key.to_string(), decode_value(raw_value.trim())?);
    }

    Ok(entries)
}

pub fn merge_env_file<I, K, V>(path: &Path, updates: I) -> io::Result<HashMap<String, String>>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut entries = read_env_file(path)?;
    for (key, value) in updates {
        entries.insert(key.into(), value.into());
    }
    write_env_file(path, &entries)?;
    Ok(entries)
}

pub fn write_env_file(path: &Path, entries: &HashMap<String, String>) -> io::Result<()> {
    let parent = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    std::fs::create_dir_all(&parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("server.env");
    let tmp_path = parent.join(format!(".{file_name}.tmp-{}", ulid::Ulid::new()));

    let mut data = entries.iter().collect::<Vec<_>>();
    data.sort_by_key(|(left, _)| *left);
    let contents = data
        .into_iter()
        .map(|(key, value)| format!("{key}={}", encode_value(value)))
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::write(&tmp_path, format!("{contents}\n"))?;
    set_private_permissions(&tmp_path)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

fn decode_value(raw: &str) -> io::Result<String> {
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        return serde_json::from_str(raw).map_err(|err| invalid_data(err.to_string()));
    }

    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        return Ok(raw[1..raw.len() - 1].to_string());
    }

    Ok(raw.to_string())
}

fn encode_value(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '+' | '='))
    {
        value.to_string()
    } else {
        serde_json::to_string(value).expect("serializing env value should not fail")
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_missing_env_file_returns_empty_map() {
        let dir = tempfile::tempdir().unwrap();
        let entries = read_env_file(&dir.path().join("server.env")).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn merge_env_file_preserves_existing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.env");
        std::fs::write(&path, "EXISTING=value\n").unwrap();

        let entries = merge_env_file(&path, [
            ("SESSION_SECRET", "secret"),
            ("FABRO_JWT_PUBLIC_KEY", "jwt"),
        ])
        .unwrap();

        assert_eq!(entries.get("EXISTING").map(String::as_str), Some("value"));
        assert_eq!(
            entries.get("SESSION_SECRET").map(String::as_str),
            Some("secret")
        );
        assert_eq!(
            entries.get("FABRO_JWT_PUBLIC_KEY").map(String::as_str),
            Some("jwt")
        );
    }

    #[test]
    fn write_env_file_round_trips_quoted_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.env");
        let entries = HashMap::from([
            ("SESSION_SECRET".to_string(), "abc 123".to_string()),
            (
                "GITHUB_APP_PRIVATE_KEY".to_string(),
                "-----BEGIN KEY-----\nabc\n-----END KEY-----".to_string(),
            ),
        ]);

        write_env_file(&path, &entries).unwrap();

        let reloaded = read_env_file(&path).unwrap();
        assert_eq!(reloaded, entries);
    }
}
