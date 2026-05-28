use std::path::Path;

use crate::Result;

#[path = "../migrations/2026050101_legacy_sandbox_to_environments.rs"]
mod legacy_sandbox_to_environments;
#[path = "../migrations/2026052801_settings_environments_to_server_files.rs"]
mod settings_environments_to_server_files;

pub(crate) use settings_environments_to_server_files::SettingsEnvironmentsMigrationReport as MigrationReport;

pub(crate) fn run_migrations(
    path: &Path,
    original_contents: &str,
) -> Result<Option<MigrationReport>> {
    settings_environments_to_server_files::migrate_settings_path(path, original_contents)
}
