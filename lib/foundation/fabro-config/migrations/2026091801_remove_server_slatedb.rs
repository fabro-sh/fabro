//! Temporary compatibility migration: drop `[server.slatedb]` from a
//! settings file.
//!
//! Earlier releases kept an embedded SlateDB store beside the artifact
//! store and configured it under `[server.slatedb]`. Run history and blobs
//! live in SQLite now and the section has no store behind it, so the
//! settings layer no longer knows the key and would refuse the file. This
//! migration removes the section (and its `local` and `s3` subtables) once,
//! with a backup beside the file, and leaves every other key as it was.
//!
//! Delete this migration after `REMOVAL_DEADLINE`, once supported upgrade
//! windows no longer start from a release that wrote the section.

#![expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "temporary startup config migration uses synchronous file I/O before config is loaded"
)]

use std::io::Write;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

use crate::{Error, Result};

/// When this migration can be removed.
pub(crate) const REMOVAL_DEADLINE: &str = "2027-03-01";

#[derive(Debug)]
pub(crate) struct RemoveServerSlateDbReport {
    pub(crate) contents: String,
    pub(crate) warning:  String,
    #[cfg(test)]
    backup_path:         PathBuf,
}

/// Rewrite `path` without its `[server.slatedb]` section when it has one:
/// the backup is written first, then the file. `Ok(None)` when the file
/// has no such section, or is not TOML the layer could read anyway.
pub(crate) fn migrate_settings_path(
    path: &Path,
    contents: &str,
) -> Result<Option<RemoveServerSlateDbReport>> {
    let Some(next_contents) = migrate_contents(contents) else {
        return Ok(None);
    };
    let backup_path = write_next_backup(path, contents)?;
    std::fs::write(path, &next_contents).map_err(|source| {
        Error::other(format!(
            "writing migrated settings file {}: {source}",
            path.display()
        ))
    })?;
    let warning = format!(
        "Removed the [server.slatedb] section from {}: Fabro no longer keeps a SlateDB store. Backup written to {}. This temporary compatibility migration will be removed after {REMOVAL_DEADLINE}.",
        path.display(),
        backup_path.display()
    );
    Ok(Some(RemoveServerSlateDbReport {
        contents: next_contents,
        warning,
        #[cfg(test)]
        backup_path,
    }))
}

/// The contents without `[server.slatedb]`, or `None` when there is
/// nothing to remove.
pub(crate) fn migrate_contents(contents: &str) -> Option<String> {
    let mut doc = contents.parse::<DocumentMut>().ok()?;
    let server = doc.get_mut("server")?.as_table_like_mut()?;
    server.remove("slatedb")?;
    Some(doc.to_string())
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
                        "writing server.slatedb migration backup {}: {source}",
                        backup_path.display()
                    ))
                })?;
                return Ok(backup_path);
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(Error::other(format!(
                    "writing server.slatedb migration backup {}: {source}",
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
        path.with_file_name(format!("{file_name}.server-slatedb-migration.bak"))
    } else {
        path.with_file_name(format!("{file_name}.server-slatedb-migration.{index}.bak"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = r#"_version = 1

[server.artifacts]
provider = "local"
prefix = "artifacts"

# The store that is gone.
[server.slatedb]
provider = "s3"
prefix = "slatedb"
disk_cache = true

[server.slatedb.s3]
bucket = "fabro-data"
region = "us-east-1"

[server.scheduler]
max_concurrent_runs = 3
"#;

    #[test]
    fn a_file_without_the_section_is_left_alone() {
        assert_eq!(
            migrate_contents("_version = 1\n\n[server.web]\nenabled = true\n"),
            None
        );
        assert_eq!(migrate_contents("not toml ["), None);
    }

    #[test]
    fn the_section_and_its_subtables_go_and_the_rest_stays() {
        let migrated = migrate_contents(LEGACY).expect("the section is removed");
        assert!(!migrated.contains("slatedb"), "{migrated}");
        assert!(migrated.contains("[server.artifacts]"), "{migrated}");
        assert!(migrated.contains("prefix = \"artifacts\""), "{migrated}");
        assert!(migrated.contains("max_concurrent_runs = 3"), "{migrated}");
        assert_eq!(
            migrate_contents(&migrated),
            None,
            "a second pass is a no-op"
        );
    }

    #[test]
    fn the_file_is_rewritten_with_a_backup_once() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, LEGACY).expect("the legacy file");

        let report = migrate_settings_path(&path, LEGACY)
            .expect("the migration runs")
            .expect("the file changed");
        assert_eq!(
            std::fs::read_to_string(&report.backup_path).expect("the backup"),
            LEGACY
        );
        let rewritten = std::fs::read_to_string(&path).expect("the file");
        assert_eq!(rewritten, report.contents);
        assert!(!rewritten.contains("slatedb"));
        assert!(report.warning.contains("[server.slatedb]"));

        assert!(
            migrate_settings_path(&path, &rewritten)
                .expect("the migration runs")
                .is_none(),
            "the second run is a no-op"
        );
    }
}
