use std::collections::HashMap;
use std::path::Path;

use fabro_vault::Vault;

#[path = "../migrations/2026051801_legacy_vault_entries.rs"]
mod legacy_vault_entries;
#[path = "../migrations/2026052501_optional_server_env_secrets_to_vault.rs"]
mod optional_server_env_secrets_to_vault;

pub(crate) use legacy_vault_entries::REMOVAL_DEADLINE as LEGACY_VAULT_REMOVAL_DEADLINE;
pub(crate) use optional_server_env_secrets_to_vault::REMOVAL_DEADLINE as OPTIONAL_SERVER_ENV_SECRETS_REMOVAL_DEADLINE;

pub(crate) type LegacyVaultMigrationReport = legacy_vault_entries::LegacyVaultMigrationReport;
pub(crate) type OptionalServerEnvSecretsMigrationReport =
    optional_server_env_secrets_to_vault::OptionalServerEnvSecretsMigrationReport;

pub(crate) fn migrate_legacy_vault_file(path: &Path) -> anyhow::Result<LegacyVaultMigrationReport> {
    legacy_vault_entries::migrate_legacy_vault_file(path)
}

pub(crate) fn migrate_optional_server_env_secrets_to_vault(
    vault: &mut Vault,
    server_env_path: &Path,
    env_entries: &HashMap<String, String>,
) -> anyhow::Result<OptionalServerEnvSecretsMigrationReport> {
    optional_server_env_secrets_to_vault::migrate(vault, server_env_path, env_entries)
}
