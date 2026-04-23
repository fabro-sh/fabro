#![expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "sync env-file load/save used at CLI and server startup; not on a Tokio path"
)]

use std::collections::HashMap;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFileUpdate {
    pub key:     String,
    pub value:   String,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFileRemoval {
    pub key:     String,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvFileEntry {
    value:   String,
    comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvFileRecord {
    key:     String,
    value:   String,
    comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFileUpdateReport {
    pub entries:      HashMap<String, String>,
    pub removed_keys: Vec<String>,
}

pub fn read_env_file(path: &Path) -> io::Result<HashMap<String, String>> {
    Ok(records_to_values(&read_env_records(path)?))
}

pub fn merge_env_file<I, K, V>(path: &Path, updates: I) -> io::Result<HashMap<String, String>>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut entries = read_env_entries(path)?;
    for (key, value) in updates {
        entries.insert(key.into(), EnvFileEntry {
            value:   value.into(),
            comment: None,
        });
    }
    write_env_entries(path, &entries)?;
    Ok(entries
        .into_iter()
        .map(|(key, entry)| (key, entry.value))
        .collect())
}

pub fn update_env_file<I, J>(
    path: &Path,
    removals: I,
    updates: J,
) -> io::Result<HashMap<String, String>>
where
    I: IntoIterator<Item = EnvFileRemoval>,
    J: IntoIterator<Item = EnvFileUpdate>,
{
    Ok(update_env_file_with_report(path, removals, updates)?.entries)
}

pub fn write_env_file(path: &Path, entries: &HashMap<String, String>) -> io::Result<()> {
    let entries = entries
        .iter()
        .map(|(key, value)| {
            (key.clone(), EnvFileEntry {
                value:   value.clone(),
                comment: None,
            })
        })
        .collect::<HashMap<_, _>>();
    write_env_entries(path, &entries)
}

pub fn update_env_file_with_report<I, J>(
    path: &Path,
    removals: I,
    updates: J,
) -> io::Result<EnvFileUpdateReport>
where
    I: IntoIterator<Item = EnvFileRemoval>,
    J: IntoIterator<Item = EnvFileUpdate>,
{
    let mut records = read_env_records(path)?;
    let mut removed_keys = Vec::new();

    for removal in removals {
        let mut removed_this_key = false;
        records.retain(|record| {
            let should_remove = record.key == removal.key
                && (removal.comment.is_none() || record.comment == removal.comment);
            if should_remove {
                removed_this_key = true;
            }
            !should_remove
        });
        if removed_this_key && !removed_keys.contains(&removal.key) {
            removed_keys.push(removal.key);
        }
    }

    for update in updates {
        match update.comment.as_deref() {
            Some(comment) => {
                records.retain(|record| {
                    !(record.key == update.key && record.comment.as_deref() == Some(comment))
                });
            }
            None => {
                records.retain(|record| record.key != update.key);
            }
        }
        records.push(EnvFileRecord {
            key:     update.key,
            value:   update.value,
            comment: update.comment,
        });
    }

    if records.is_empty() {
        remove_optional_file(path)?;
        return Ok(EnvFileUpdateReport {
            entries: HashMap::new(),
            removed_keys,
        });
    }

    write_env_records(path, &records)?;
    Ok(EnvFileUpdateReport {
        entries: records_to_values(&records),
        removed_keys,
    })
}

fn read_env_entries(path: &Path) -> io::Result<HashMap<String, EnvFileEntry>> {
    Ok(records_to_entries(&read_env_records(path)?))
}

fn read_env_records(path: &Path) -> io::Result<Vec<EnvFileRecord>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let mut records = Vec::new();
    let mut pending_comment: Option<String> = None;
    for (index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            pending_comment = None;
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            pending_comment = Some(comment.trim().to_string());
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

        records.push(EnvFileRecord {
            key:     key.to_string(),
            value:   decode_value(raw_value.trim())?,
            comment: pending_comment.take(),
        });
    }

    Ok(records)
}

fn write_env_entries(path: &Path, entries: &HashMap<String, EnvFileEntry>) -> io::Result<()> {
    let records = entries
        .iter()
        .map(|(key, entry)| EnvFileRecord {
            key:     key.clone(),
            value:   entry.value.clone(),
            comment: entry.comment.clone(),
        })
        .collect::<Vec<_>>();
    write_env_records(path, &records)
}

fn write_env_records(path: &Path, records: &[EnvFileRecord]) -> io::Result<()> {
    let parent = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    std::fs::create_dir_all(&parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("server.env");
    let tmp_path = parent.join(format!(".{file_name}.tmp-{}", ulid::Ulid::new()));

    let mut data = records.to_vec();
    data.sort_by(|left, right| left.key.cmp(&right.key));
    let mut contents = String::new();
    for record in data {
        if let Some(comment) = record.comment.as_deref() {
            if comment.contains('\n') {
                return Err(invalid_data(format!(
                    "env comments must be single-line in {}",
                    path.display()
                )));
            }
            contents.push_str("# ");
            contents.push_str(comment);
            contents.push('\n');
        }
        contents.push_str(&record.key);
        contents.push('=');
        contents.push_str(&encode_value(&record.value));
        contents.push('\n');
    }

    let mut file = std::fs::File::create(&tmp_path)?;
    file.write_all(contents.as_bytes())?;
    set_private_permissions(&tmp_path)?;
    file.sync_all()?;
    std::fs::rename(&tmp_path, path)?;
    sync_parent_directory(&parent)?;
    Ok(())
}

fn records_to_entries(records: &[EnvFileRecord]) -> HashMap<String, EnvFileEntry> {
    records
        .iter()
        .cloned()
        .map(|record| {
            (record.key, EnvFileEntry {
                value:   record.value,
                comment: record.comment,
            })
        })
        .collect()
}

fn records_to_values(records: &[EnvFileRecord]) -> HashMap<String, String> {
    records
        .iter()
        .cloned()
        .map(|record| (record.key, record.value))
        .collect()
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

fn remove_optional_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            let parent = path
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            sync_parent_directory(&parent)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
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
            ("FABRO_DEV_TOKEN", "token"),
        ])
        .unwrap();

        assert_eq!(entries.get("EXISTING").map(String::as_str), Some("value"));
        assert_eq!(
            entries.get("SESSION_SECRET").map(String::as_str),
            Some("secret")
        );
        assert_eq!(
            entries.get("FABRO_DEV_TOKEN").map(String::as_str),
            Some("token")
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

    #[test]
    fn update_env_file_only_removes_matching_marked_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.env");
        std::fs::write(
            &path,
            "AWS_ACCESS_KEY_ID=operator\n# managed by fabro-install: object-store\nAWS_ACCESS_KEY_ID=managed\n# managed by fabro-install: object-store\nAWS_SECRET_ACCESS_KEY=managed-secret\nKEEP_ME=1\n",
        )
        .unwrap();

        let report = update_env_file_with_report(
            &path,
            [EnvFileRemoval {
                key:     "AWS_ACCESS_KEY_ID".to_string(),
                comment: Some("managed by fabro-install: object-store".to_string()),
            }],
            [EnvFileUpdate {
                key:     "AWS_SECRET_ACCESS_KEY".to_string(),
                value:   "replaced".to_string(),
                comment: Some("managed by fabro-install: object-store".to_string()),
            }],
        )
        .unwrap();
        let entries = report.entries;

        assert_eq!(
            entries.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
            Some("replaced")
        );
        assert_eq!(
            entries.get("AWS_ACCESS_KEY_ID").map(String::as_str),
            Some("operator")
        );
        assert_eq!(entries.get("KEEP_ME").map(String::as_str), Some("1"));
        assert_eq!(report.removed_keys, vec!["AWS_ACCESS_KEY_ID".to_string()]);
    }

    #[test]
    fn update_env_file_reports_only_keys_actually_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.env");
        std::fs::write(
            &path,
            "# managed by fabro-install: object-store\nAWS_ACCESS_KEY_ID=managed\nKEEP_ME=1\n",
        )
        .unwrap();

        let report = update_env_file_with_report(
            &path,
            [
                EnvFileRemoval {
                    key:     "AWS_ACCESS_KEY_ID".to_string(),
                    comment: Some("managed by fabro-install: object-store".to_string()),
                },
                EnvFileRemoval {
                    key:     "AWS_SECRET_ACCESS_KEY".to_string(),
                    comment: Some("managed by fabro-install: object-store".to_string()),
                },
            ],
            [],
        )
        .unwrap();

        assert_eq!(report.removed_keys, vec!["AWS_ACCESS_KEY_ID".to_string()]);
        assert_eq!(report.entries.get("KEEP_ME").map(String::as_str), Some("1"));
    }
}
