use std::path::Path;

use crate::Result;

#[path = "../migrations/2026050101_legacy_sandbox_to_environments.rs"]
mod legacy_sandbox_to_environments;

pub(crate) type MigrationReport = legacy_sandbox_to_environments::LegacySandboxMigrationReport;

pub(crate) fn run_migrations(
    path: &Path,
    original_contents: &str,
) -> Result<Option<MigrationReport>> {
    legacy_sandbox_to_environments::migrate_settings_path(path, original_contents)
}
