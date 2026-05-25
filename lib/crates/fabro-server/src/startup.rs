use std::collections::HashMap;
use std::path::Path;

use anyhow::Context as _;
use fabro_static::EnvVars;
use fabro_types::settings::ServerNamespace;
use fabro_vault::Vault;
use tracing::warn;

use crate::jwt_auth::{AuthMode, resolve_auth_mode_with_lookup};
use crate::server_secrets::ServerSecrets;
use crate::vault_legacy_migration;

pub(crate) fn resolve_startup(
    env_path: &Path,
    env_entries: HashMap<String, String>,
    settings: &ServerNamespace,
    vault: &Vault,
) -> anyhow::Result<(AuthMode, ServerSecrets)> {
    let server_secrets = ServerSecrets::load(env_path, env_entries)?;
    let auth_secret_lookup = |name: &str| match name {
        EnvVars::GITHUB_APP_CLIENT_SECRET => vault.get(name).map(str::to_string),
        _ => server_secrets.get(name),
    };
    let auth_mode = resolve_auth_mode_with_lookup(settings, auth_secret_lookup)?;
    Ok((auth_mode, server_secrets))
}

pub fn load_startup_vault(vault_path: impl AsRef<Path>) -> anyhow::Result<Vault> {
    let vault_path = vault_path.as_ref();
    match vault_legacy_migration::migrate_legacy_vault_file(vault_path) {
        Ok(report) if report.changed() => {
            let backup_path = report
                .backup_path
                .as_ref()
                .map_or_else(|| "<none>".to_string(), |path| path.display().to_string());
            warn!(
                migrated_entries = report.migrated_entries,
                skipped_entries = report.skipped_entries,
                backup_path = %backup_path,
                removal_deadline = vault_legacy_migration::REMOVAL_DEADLINE,
                "Migrated legacy vault file"
            );
        }
        Ok(_) => {}
        Err(err) => {
            warn!(
                error = %err,
                removal_deadline = vault_legacy_migration::REMOVAL_DEADLINE,
                "Legacy vault migration failed; continuing with normal vault load"
            );
        }
    }
    Vault::load(vault_path.to_path_buf())
        .with_context(|| format!("load vault {}", vault_path.display()))
}

pub fn validate_startup(
    env_path: &Path,
    env_entries: HashMap<String, String>,
    settings: &ServerNamespace,
    vault: &Vault,
) -> anyhow::Result<()> {
    resolve_startup(env_path, env_entries, settings, vault).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_config::ServerSettingsBuilder;
    use fabro_static::EnvVars;
    use fabro_types::settings::ServerNamespace;
    use fabro_vault::{SecretType, Vault};

    use super::validate_startup;

    fn resolved_settings(auth_methods: &[&str]) -> ServerNamespace {
        ServerSettingsBuilder::from_toml(&format!(
            r#"
_version = 1

[server.auth]
methods = [{}]

[server.auth.github]
allowed_usernames = ["octocat"]

[server.integrations.github]
client_id = "Iv1.test"
"#,
            auth_methods
                .iter()
                .map(|method| format!("\"{method}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .unwrap()
        .server
    }

    fn empty_vault(dir: &tempfile::TempDir) -> Vault {
        Vault::load(dir.path().join("secrets.json")).unwrap()
    }

    #[test]
    fn validate_startup_accepts_configured_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let vault = empty_vault(&dir);
        let env = HashMap::from([
            (
                EnvVars::SESSION_SECRET.to_string(),
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            ),
            (
                EnvVars::FABRO_DEV_TOKEN.to_string(),
                "fabro_dev_abababababababababababababababababababababababababababababababab"
                    .to_string(),
            ),
        ]);
        let settings = resolved_settings(&["dev-token"]);

        assert!(
            validate_startup(
                dir.path().join("server.env").as_path(),
                env,
                &settings,
                &vault,
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_startup_rejects_missing_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let settings = resolved_settings(&["dev-token"]);
        let vault = empty_vault(&dir);

        assert!(
            validate_startup(
                dir.path().join("server.env").as_path(),
                HashMap::new(),
                &settings,
                &vault,
            )
            .is_err()
        );
    }

    #[test]
    fn validate_startup_requires_github_client_secret_from_vault() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([
            (
                EnvVars::SESSION_SECRET.to_string(),
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            ),
            (
                EnvVars::GITHUB_APP_CLIENT_SECRET.to_string(),
                "server-env-client-secret".to_string(),
            ),
        ]);
        let settings = resolved_settings(&["github"]);
        let vault = empty_vault(&dir);

        let err = validate_startup(
            dir.path().join("server.env").as_path(),
            env,
            &settings,
            &vault,
        )
        .expect_err("github client secret in server.env should not satisfy startup");

        assert!(err.to_string().contains("GITHUB_APP_CLIENT_SECRET"));
    }

    #[test]
    fn validate_startup_accepts_github_client_secret_from_vault() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([(
            EnvVars::SESSION_SECRET.to_string(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        )]);
        let settings = resolved_settings(&["github"]);
        let mut vault = empty_vault(&dir);
        vault
            .set(
                EnvVars::GITHUB_APP_CLIENT_SECRET,
                "vault-client-secret",
                SecretType::Token,
                None,
            )
            .unwrap();

        validate_startup(
            dir.path().join("server.env").as_path(),
            env,
            &settings,
            &vault,
        )
        .expect("github client secret in vault should satisfy startup");
    }
}
