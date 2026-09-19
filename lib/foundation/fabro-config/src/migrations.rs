//! The settings-file migrations, in the order they run: the legacy
//! `[run.sandbox]` and `[environments]` rewrites first, then the removal of
//! `[server.slatedb]`. Each is state-driven and a no-op on a file already
//! in the current shape.

use std::path::Path;

use crate::Result;

#[path = "../migrations/2026050101_legacy_sandbox_to_environments.rs"]
mod legacy_sandbox_to_environments;
#[path = "../migrations/2026091801_remove_server_slatedb.rs"]
mod remove_server_slatedb;
#[path = "../migrations/2026052801_settings_environments_to_server_files.rs"]
mod settings_environments_to_server_files;

/// What the migrations left: the file's contents now, and the warning that
/// names every rewrite.
#[derive(Debug)]
pub(crate) struct MigrationReport {
    pub(crate) contents: String,
    pub(crate) warning:  String,
}

pub(crate) fn run_migrations(
    path: &Path,
    original_contents: &str,
) -> Result<Option<MigrationReport>> {
    let mut report: Option<MigrationReport> = None;
    if let Some(environments) =
        settings_environments_to_server_files::migrate_settings_path(path, original_contents)?
    {
        report = Some(MigrationReport {
            contents: environments.contents,
            warning:  environments.warning,
        });
    }
    let contents = report
        .as_ref()
        .map_or(original_contents, |report| report.contents.as_str());
    if let Some(slatedb) = remove_server_slatedb::migrate_settings_path(path, contents)? {
        report = Some(match report {
            Some(earlier) => MigrationReport {
                contents: slatedb.contents,
                warning:  format!("{} {}", earlier.warning, slatedb.warning),
            },
            None => MigrationReport {
                contents: slatedb.contents,
                warning:  slatedb.warning,
            },
        });
    }
    Ok(report)
}
