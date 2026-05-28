#![expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "temporary startup config migration uses synchronous file I/O before config is loaded"
)]

use std::io::Write;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use crate::{Error, Result};

pub(crate) const REMOVAL_NOTE: &str =
    "This temporary compatibility migration will be removed before v1.0.";

#[derive(Debug)]
pub(crate) struct SettingsEnvironmentsMigrationReport {
    pub(crate) contents: String,
    pub(crate) warning:  String,
    #[cfg(test)]
    backup_path:         PathBuf,
}

pub(crate) fn migrate_settings_path(
    path: &Path,
    original_contents: &str,
) -> Result<Option<SettingsEnvironmentsMigrationReport>> {
    let legacy_migrated =
        super::legacy_sandbox_to_environments::migrate_contents(original_contents, path)?;
    let contents = legacy_migrated.as_deref().unwrap_or(original_contents);
    let legacy_changed = legacy_migrated.is_some();

    let Some((next_contents, extracted)) = extract_environments(contents, path)? else {
        if legacy_changed {
            return Err(Error::other(format!(
                "Legacy [run.sandbox] settings in {} did not produce a server environment file candidate.",
                path.display()
            )));
        }
        return Ok(None);
    };

    let settings_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let environment_dir = settings_dir.join("environments");
    let existing = extracted
        .iter()
        .map(|environment| environment_dir.join(format!("{}.toml", environment.id)))
        .find(|target| target.exists());
    if let Some(target) = existing {
        return Err(Error::other(format!(
            "Settings environments in {} could not be auto-migrated because target environment file {} already exists. Move or merge the file and retry.",
            path.display(),
            target.display()
        )));
    }

    let backup_path = write_next_backup(path, original_contents)?;
    std::fs::create_dir_all(&environment_dir).map_err(|source| {
        Error::other(format!(
            "creating server environments directory {}: {source}",
            environment_dir.display()
        ))
    })?;
    for environment in &extracted {
        let target = environment_dir.join(format!("{}.toml", environment.id));
        write_new_file(&target, &environment.contents)?;
    }
    std::fs::write(path, &next_contents).map_err(|source| {
        Error::other(format!(
            "writing migrated settings file {}: {source}",
            path.display()
        ))
    })?;

    let ids = extracted
        .iter()
        .map(|environment| environment.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let warning = format!(
        "Migrated [environments] settings in {} to server environment files under {} ({ids}). Backup written to {}. {REMOVAL_NOTE}",
        path.display(),
        environment_dir.display(),
        backup_path.display()
    );

    Ok(Some(SettingsEnvironmentsMigrationReport {
        contents: next_contents,
        warning,
        #[cfg(test)]
        backup_path,
    }))
}

#[derive(Debug)]
struct ExtractedEnvironment {
    id:       String,
    contents: String,
}

fn extract_environments(
    contents: &str,
    path: &Path,
) -> Result<Option<(String, Vec<ExtractedEnvironment>)>> {
    let Ok(mut doc) = contents.parse::<DocumentMut>() else {
        return Ok(None);
    };
    let Some(environments_item) = doc.get("environments") else {
        return Ok(None);
    };
    let Some(environments) = environments_item.as_table() else {
        return Err(Error::other(format!(
            "[environments] in {} must be a table to migrate to server environment files.",
            path.display()
        )));
    };

    let mut extracted = Vec::new();
    for (id, item) in environments {
        validate_environment_id(id, path)?;
        let contents = environment_file_contents(id, item, path)?;
        extracted.push(ExtractedEnvironment {
            id: id.to_string(),
            contents,
        });
    }
    if extracted.is_empty() {
        return Ok(None);
    }
    extracted.sort_by(|left, right| left.id.cmp(&right.id));

    doc.as_table_mut().remove("environments");
    Ok(Some((doc.to_string(), extracted)))
}

fn environment_file_contents(id: &str, item: &Item, path: &Path) -> Result<String> {
    let Some(table) = item.as_table() else {
        return Err(Error::other(format!(
            "[environments.{id}] in {} must be a table to migrate to a server environment file.",
            path.display()
        )));
    };

    let mut doc = DocumentMut::new();
    for (key, value) in table {
        doc[key] = value.clone();
    }
    Ok(doc.to_string())
}

fn validate_environment_id(id: &str, path: &Path) -> Result<()> {
    if is_valid_environment_id(id) {
        Ok(())
    } else {
        Err(Error::other(format!(
            "[environments.{id}] in {} could not be migrated because environment ids must match [a-z0-9][a-z0-9-]{{0,62}}.",
            path.display()
        )))
    }
}

fn is_valid_environment_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    if value.len() > 63 {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn write_new_file(path: &Path, contents: &str) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| {
            Error::other(format!(
                "writing server environment file {}: {source}",
                path.display()
            ))
        })?;
    file.write_all(contents.as_bytes()).map_err(|source| {
        Error::other(format!(
            "writing server environment file {}: {source}",
            path.display()
        ))
    })?;
    file.sync_all().map_err(|source| {
        Error::other(format!(
            "syncing server environment file {}: {source}",
            path.display()
        ))
    })?;
    Ok(())
}

fn write_next_backup(path: &Path, contents: &str) -> Result<PathBuf> {
    for index in 0u32.. {
        let backup_path = backup_path_for(path, index);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
        {
            Ok(mut file) => {
                file.write_all(contents.as_bytes()).map_err(|source| {
                    Error::other(format!(
                        "writing settings environments migration backup {}: {source}",
                        backup_path.display()
                    ))
                })?;
                return Ok(backup_path);
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(Error::other(format!(
                    "writing settings environments migration backup {}: {source}",
                    backup_path.display()
                )));
            }
        }
    }
    unreachable!("unbounded backup suffix search should return")
}

fn backup_path_for(path: &Path, index: u32) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.toml");
    if index == 0 {
        path.with_file_name(format!("{file_name}.settings-environments-migration.bak"))
    } else {
        path.with_file_name(format!(
            "{file_name}.settings-environments-migration.{index}.bak"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_environments_and_removes_catalog_from_settings() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        let source = r#"
_version = 1

[run.environment]
id = "cloud"

[environments.cloud]
provider = "docker"

[environments.cloud.image]
docker = "ubuntu:24.04"
"#;
        std::fs::write(&path, source).expect("write settings");

        let report = migrate_settings_path(&path, source)
            .expect("migration should succeed")
            .expect("catalog should migrate");

        let rewritten = std::fs::read_to_string(&path).expect("read settings");
        let environment =
            std::fs::read_to_string(dir.path().join("environments/cloud.toml")).expect("read env");
        assert!(report.backup_path.exists());
        assert!(!rewritten.contains("[environments"));
        assert!(rewritten.contains("id = \"cloud\""));
        assert!(environment.contains("provider = \"docker\""));
        assert!(environment.contains("docker = \"ubuntu:24.04\""));
    }

    #[test]
    fn target_conflict_fails_without_changing_settings_or_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        let environment_dir = dir.path().join("environments");
        std::fs::create_dir_all(&environment_dir).expect("mkdir");
        std::fs::write(environment_dir.join("cloud.toml"), "provider = \"local\"\n")
            .expect("write existing");
        let source = r#"
_version = 1

[environments.cloud]
provider = "docker"
"#;
        std::fs::write(&path, source).expect("write settings");

        let err = migrate_settings_path(&path, source).expect_err("conflict should fail");

        assert!(err.to_string().contains("already exists"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert_eq!(
            std::fs::read_to_string(environment_dir.join("cloud.toml")).unwrap(),
            "provider = \"local\"\n"
        );
    }

    #[test]
    fn legacy_sandbox_is_chained_into_default_environment_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        let source = r#"
_version = 1

[run.sandbox]
provider = "docker"
preserve = true
"#;
        std::fs::write(&path, source).expect("write settings");

        migrate_settings_path(&path, source)
            .expect("migration should succeed")
            .expect("legacy sandbox should migrate");

        let rewritten = std::fs::read_to_string(&path).expect("read settings");
        let environment = std::fs::read_to_string(dir.path().join("environments/default.toml"))
            .expect("read default env");
        assert!(rewritten.contains("[run.environment]"));
        assert!(rewritten.contains("id = \"default\""));
        assert!(!rewritten.contains("[environments"));
        assert!(environment.contains("provider = \"docker\""));
        assert!(environment.contains("preserve = true"));
    }
}
