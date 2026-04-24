#![expect(
    clippy::disallowed_methods,
    reason = "fabro-install: sync CLI install/uninstall bookkeeping; not on a Tokio hot path"
)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fabro_config::{Storage, envfile};
use fabro_static::EnvVars;
use fabro_vault::{SecretType as VaultSecretType, Vault};

pub struct PendingSettingsWrite<'a> {
    pub path:              &'a Path,
    pub contents:          &'a str,
    pub previous_contents: Option<&'a str>,
}

pub const OBJECT_STORE_MANAGED_COMMENT: &str = "managed by fabro-install: object-store";
pub const OBJECT_STORE_ACCESS_KEY_ID_ENV: &str = EnvVars::AWS_ACCESS_KEY_ID;
pub const OBJECT_STORE_SECRET_ACCESS_KEY_ENV: &str = EnvVars::AWS_SECRET_ACCESS_KEY;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultSecretWrite {
    pub name:        String,
    pub value:       String,
    pub secret_type: VaultSecretType,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallListenConfig {
    Tcp(String),
    Unix(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallObjectStoreCredentialMode {
    Runtime,
    AccessKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallObjectStoreSelection {
    Local,
    S3 {
        bucket:            String,
        region:            String,
        credential_mode:   InstallObjectStoreCredentialMode,
        access_key_id:     Option<String>,
        secret_access_key: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallObjectStoreEnvPlan {
    pub writes:   Vec<envfile::EnvFileUpdate>,
    pub removals: Vec<envfile::EnvFileRemoval>,
}

#[derive(Debug)]
pub struct PersistInstallOutputsError {
    source:                 anyhow::Error,
    pub server_env_applied: bool,
    pub removed_env_keys:   Vec<String>,
}

impl PersistInstallOutputsError {
    fn new(source: anyhow::Error, server_env_applied: bool, removed_env_keys: Vec<String>) -> Self {
        Self {
            source,
            server_env_applied,
            removed_env_keys,
        }
    }
}

impl std::fmt::Display for PersistInstallOutputsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for PersistInstallOutputsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

pub fn default_web_url() -> String {
    "http://127.0.0.1:32276".to_string()
}

fn root_table_mut(doc: &mut toml::Value) -> Result<&mut toml::Table> {
    doc.as_table_mut()
        .context("settings.toml root is not a table")
}

fn ensure_table<'a>(table: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table> {
    table
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::default()))
        .as_table_mut()
        .with_context(|| format!("settings.toml [{key}] is not a table"))
}

fn github_integration_table(doc: &mut toml::Value) -> Result<&mut toml::Table> {
    let root = doc
        .as_table_mut()
        .context("settings.toml root is not a table")?;
    let server = root
        .entry("server")
        .or_insert_with(|| toml::Value::Table(toml::Table::default()));
    let server_table = server
        .as_table_mut()
        .context("settings.toml [server] is not a table")?;
    let integrations = server_table
        .entry("integrations")
        .or_insert_with(|| toml::Value::Table(toml::Table::default()));
    let integrations_table = integrations
        .as_table_mut()
        .context("settings.toml [server.integrations] is not a table")?;
    let github = integrations_table
        .entry("github")
        .or_insert_with(|| toml::Value::Table(toml::Table::default()));
    github
        .as_table_mut()
        .context("settings.toml [server.integrations.github] is not a table")
}

pub fn merge_server_settings(
    doc: &mut toml::Value,
    web_url: &str,
    listen_config: &InstallListenConfig,
) -> Result<()> {
    let root = root_table_mut(doc)?;
    root.insert("_version".to_string(), toml::Value::Integer(1));

    let server = ensure_table(root, "server")?;

    let api = ensure_table(server, "api")?;
    api.insert(
        "url".to_string(),
        toml::Value::String(format!("{web_url}/api/v1")),
    );

    let listen = ensure_table(server, "listen")?;
    match listen_config {
        InstallListenConfig::Tcp(address) => {
            listen.insert("type".to_string(), toml::Value::String("tcp".to_string()));
            listen.insert("address".to_string(), toml::Value::String(address.clone()));
            listen.remove("path");
        }
        InstallListenConfig::Unix(path) => {
            listen.insert("type".to_string(), toml::Value::String("unix".to_string()));
            listen.insert(
                "path".to_string(),
                toml::Value::String(path.display().to_string()),
            );
            listen.remove("address");
        }
    }

    let web = ensure_table(server, "web")?;
    web.insert("enabled".to_string(), toml::Value::Boolean(true));
    web.insert("url".to_string(), toml::Value::String(web_url.to_string()));

    let auth = ensure_table(server, "auth")?;
    auth.insert(
        "methods".to_string(),
        toml::Value::Array(vec![toml::Value::String("dev-token".to_string())]),
    );

    let cli = ensure_table(root, "cli")?;
    let target = ensure_table(cli, "target")?;
    target.insert("type".to_string(), toml::Value::String("http".to_string()));
    target.insert("url".to_string(), toml::Value::String(web_url.to_string()));

    Ok(())
}

pub fn write_token_settings(doc: &mut toml::Value) -> Result<()> {
    if let Some(server) = doc.get_mut("server").and_then(toml::Value::as_table_mut) {
        if let Some(auth) = server.get_mut("auth").and_then(toml::Value::as_table_mut) {
            if let Some(methods) = auth.get_mut("methods").and_then(toml::Value::as_array_mut) {
                methods.retain(|value| value.as_str() != Some("github"));
                if methods.is_empty() {
                    methods.push(toml::Value::String("dev-token".to_string()));
                }
            }
            auth.remove("github");
        }
    }

    let github = github_integration_table(doc)?;
    github.insert("strategy".into(), toml::Value::String("token".to_string()));
    github.remove("app_id");
    github.remove("slug");
    github.remove("client_id");
    Ok(())
}

pub fn write_github_app_settings(
    doc: &mut toml::Value,
    app_id: &str,
    slug: &str,
    client_id: &str,
    allowed_usernames: &[String],
) -> Result<()> {
    anyhow::ensure!(
        !allowed_usernames.is_empty(),
        "GitHub App install requires at least one allowed GitHub username"
    );

    let root = root_table_mut(doc)?;
    let server = ensure_table(root, "server")?;
    let auth = ensure_table(server, "auth")?;
    let methods = auth
        .entry("methods".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .context("settings.toml [server.auth].methods is not an array")?;
    if !methods.iter().any(|value| value.as_str() == Some("github")) {
        methods.push(toml::Value::String("github".to_string()));
    }
    methods.retain(|value| value.as_str() != Some("dev-token"));
    let github_auth = ensure_table(auth, "github")?;
    github_auth.insert(
        "allowed_usernames".to_string(),
        toml::Value::Array(
            allowed_usernames
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );

    let github = github_integration_table(doc)?;
    github.insert("strategy".into(), toml::Value::String("app".to_string()));
    github.insert("app_id".into(), toml::Value::String(app_id.to_string()));
    github.insert("slug".into(), toml::Value::String(slug.to_string()));
    github.insert(
        "client_id".into(),
        toml::Value::String(client_id.to_string()),
    );
    Ok(())
}

fn object_store_env_removals() -> Vec<envfile::EnvFileRemoval> {
    [
        OBJECT_STORE_ACCESS_KEY_ID_ENV,
        OBJECT_STORE_SECRET_ACCESS_KEY_ENV,
    ]
    .into_iter()
    .map(|key| envfile::EnvFileRemoval {
        key:     key.to_string(),
        comment: Some(OBJECT_STORE_MANAGED_COMMENT.to_string()),
    })
    .collect()
}

fn write_s3_store_settings(
    server: &mut toml::Table,
    domain: &str,
    prefix: &str,
    bucket: &str,
    region: &str,
) -> Result<()> {
    let store = ensure_table(server, domain)?;
    store.insert(
        "provider".to_string(),
        toml::Value::String("s3".to_string()),
    );
    store.insert(
        "prefix".to_string(),
        toml::Value::String(prefix.to_string()),
    );
    let s3 = ensure_table(store, "s3")?;
    s3.insert(
        "bucket".to_string(),
        toml::Value::String(bucket.to_string()),
    );
    s3.insert(
        "region".to_string(),
        toml::Value::String(region.to_string()),
    );
    Ok(())
}

pub fn write_object_store_settings(
    doc: &mut toml::Value,
    selection: &InstallObjectStoreSelection,
) -> Result<InstallObjectStoreEnvPlan> {
    match selection {
        InstallObjectStoreSelection::Local => Ok(InstallObjectStoreEnvPlan {
            writes:   Vec::new(),
            removals: object_store_env_removals(),
        }),
        InstallObjectStoreSelection::S3 {
            bucket,
            region,
            credential_mode,
            access_key_id,
            secret_access_key,
        } => {
            let bucket = bucket.trim();
            anyhow::ensure!(!bucket.is_empty(), "bucket is required");
            let region = region.trim();
            anyhow::ensure!(!region.is_empty(), "region is required");

            let root = root_table_mut(doc)?;
            let server = ensure_table(root, "server")?;
            write_s3_store_settings(server, "artifacts", "artifacts", bucket, region)?;
            write_s3_store_settings(server, "slatedb", "slatedb", bucket, region)?;

            let removals = object_store_env_removals();
            let writes = match credential_mode {
                InstallObjectStoreCredentialMode::Runtime => Vec::new(),
                InstallObjectStoreCredentialMode::AccessKey => {
                    let access_key_id = access_key_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .context("access_key_id is required for manual credentials")?;
                    let secret_access_key = secret_access_key
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .context("secret_access_key is required for manual credentials")?;
                    vec![
                        envfile::EnvFileUpdate {
                            key:     OBJECT_STORE_ACCESS_KEY_ID_ENV.to_string(),
                            value:   access_key_id.to_string(),
                            comment: Some(OBJECT_STORE_MANAGED_COMMENT.to_string()),
                        },
                        envfile::EnvFileUpdate {
                            key:     OBJECT_STORE_SECRET_ACCESS_KEY_ENV.to_string(),
                            value:   secret_access_key.to_string(),
                            comment: Some(OBJECT_STORE_MANAGED_COMMENT.to_string()),
                        },
                    ]
                }
            };

            Ok(InstallObjectStoreEnvPlan { writes, removals })
        }
    }
}

fn restore_optional_file(path: &Path, previous_contents: Option<&str>) -> Result<()> {
    match previous_contents {
        Some(contents) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating directory {}", parent.display()))?;
            }
            std::fs::write(path, contents)
                .with_context(|| format!("restoring {}", path.display()))?;
        }
        None => match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!("removing {}", path.display())));
            }
        },
    }

    Ok(())
}

fn persist_server_env_secrets(
    storage_dir: &Path,
    writes: &[envfile::EnvFileUpdate],
    removals: &[envfile::EnvFileRemoval],
) -> Result<envfile::EnvFileUpdateReport> {
    if writes.is_empty() && removals.is_empty() {
        return Ok(envfile::EnvFileUpdateReport {
            entries:      std::collections::HashMap::new(),
            removed_keys: Vec::new(),
        });
    }

    let env_path = Storage::new(storage_dir).runtime_directory().env_path();
    envfile::update_env_file_with_report(
        &env_path,
        removals.iter().cloned(),
        writes.iter().cloned(),
    )
    .with_context(|| format!("updating server env file {}", env_path.display()))
}

fn persist_vault_secrets_direct(storage_dir: &Path, secrets: &[VaultSecretWrite]) -> Result<()> {
    if secrets.is_empty() {
        return Ok(());
    }

    let vault_path = Storage::new(storage_dir).secrets_path();
    let mut vault = Vault::load(vault_path).map_err(anyhow::Error::from)?;
    for secret in secrets {
        vault
            .set(
                &secret.name,
                &secret.value,
                secret.secret_type,
                secret.description.as_deref(),
            )
            .map_err(anyhow::Error::from)?;
    }
    Ok(())
}

pub fn persist_install_outputs_direct(
    storage_dir: &Path,
    server_env_writes: &[envfile::EnvFileUpdate],
    server_env_removals: &[envfile::EnvFileRemoval],
    vault_secrets: &[VaultSecretWrite],
    settings_write: Option<&PendingSettingsWrite<'_>>,
) -> std::result::Result<(), PersistInstallOutputsError> {
    let server_env_report =
        persist_server_env_secrets(storage_dir, server_env_writes, server_env_removals)
            .map_err(|err| PersistInstallOutputsError::new(err, false, Vec::new()))?;
    let removed_env_keys = server_env_report.removed_keys;

    if let Some(write) = settings_write {
        if let Some(parent) = write.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating settings directory {}", parent.display()))
                .map_err(|err| {
                    PersistInstallOutputsError::new(err, true, removed_env_keys.clone())
                })?;
        }
        std::fs::write(write.path, write.contents)
            .with_context(|| format!("writing settings file {}", write.path.display()))
            .map_err(|err| PersistInstallOutputsError::new(err, true, removed_env_keys.clone()))?;
    }

    let vault_path = Storage::new(storage_dir).secrets_path();
    let previous_vault = std::fs::read_to_string(&vault_path).ok();

    if let Err(err) = persist_vault_secrets_direct(storage_dir, vault_secrets) {
        let mut rollback_failures = Vec::new();
        if let Some(write) = settings_write {
            if let Err(restore_err) = restore_optional_file(write.path, write.previous_contents) {
                rollback_failures.push(restore_err.to_string());
            }
        }
        if let Err(restore_err) = restore_optional_file(&vault_path, previous_vault.as_deref()) {
            rollback_failures.push(restore_err.to_string());
        }
        let error = if rollback_failures.is_empty() {
            err.context("persisting install outputs directly")
        } else {
            err.context(format!(
                "persisting install outputs directly; rollback failures: {}",
                rollback_failures.join("; ")
            ))
        };
        return Err(PersistInstallOutputsError::new(
            error,
            true,
            removed_env_keys,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use fabro_config::{ServerSettingsBuilder, Storage, envfile};
    use fabro_vault::{SecretType as VaultSecretType, Vault};

    use super::{
        InstallListenConfig, InstallObjectStoreCredentialMode, InstallObjectStoreSelection,
        OBJECT_STORE_ACCESS_KEY_ID_ENV, OBJECT_STORE_MANAGED_COMMENT,
        OBJECT_STORE_SECRET_ACCESS_KEY_ENV, PendingSettingsWrite, VaultSecretWrite,
        default_web_url, merge_server_settings, persist_install_outputs_direct,
        write_github_app_settings, write_object_store_settings,
    };

    fn format_config_toml() -> String {
        let mut doc = toml::Value::Table(toml::Table::default());
        merge_server_settings(
            &mut doc,
            &default_web_url(),
            &InstallListenConfig::Tcp("127.0.0.1:32276".to_string()),
        )
        .expect("default server config should be valid");
        toml::to_string_pretty(&doc).expect("default server config should serialize")
    }

    #[test]
    fn config_toml_has_auth_strategies() {
        use fabro_types::settings::ServerAuthMethod;

        let toml_str = format_config_toml();
        let cfg =
            ServerSettingsBuilder::from_toml(&toml_str).expect("generated config should resolve");
        assert_eq!(cfg.server.auth.methods, vec![ServerAuthMethod::DevToken]);
    }

    #[test]
    fn merge_server_settings_preserves_existing_top_level_sections() {
        let mut doc: toml::Value = toml::from_str(
            r#"
_version = 1

[project]
name = "custom"
"#,
        )
        .unwrap();

        merge_server_settings(
            &mut doc,
            &default_web_url(),
            &InstallListenConfig::Tcp("127.0.0.1:32276".to_string()),
        )
        .unwrap();

        assert_eq!(
            doc.get("project")
                .and_then(toml::Value::as_table)
                .and_then(|project| project.get("name"))
                .and_then(toml::Value::as_str),
            Some("custom")
        );
    }

    #[test]
    fn write_github_app_settings_uses_server_integrations_github() {
        let mut doc = toml::Value::Table(toml::Table::default());
        merge_server_settings(
            &mut doc,
            &default_web_url(),
            &InstallListenConfig::Tcp("127.0.0.1:32276".to_string()),
        )
        .unwrap();

        write_github_app_settings(&mut doc, "123", "fabro-app", "client-id", &[
            "brynary".to_string()
        ])
        .unwrap();

        let github = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("integrations"))
            .and_then(toml::Value::as_table)
            .and_then(|integrations| integrations.get("github"))
            .and_then(toml::Value::as_table)
            .expect("server.integrations.github should exist");

        assert_eq!(
            github.get("strategy").and_then(toml::Value::as_str),
            Some("app")
        );
        assert_eq!(
            github.get("app_id").and_then(toml::Value::as_str),
            Some("123")
        );
        assert_eq!(
            github.get("slug").and_then(toml::Value::as_str),
            Some("fabro-app")
        );
        assert_eq!(
            github.get("client_id").and_then(toml::Value::as_str),
            Some("client-id")
        );

        let methods = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("auth"))
            .and_then(toml::Value::as_table)
            .and_then(|auth| auth.get("methods"))
            .and_then(toml::Value::as_array)
            .expect("server.auth.methods should exist");
        assert_eq!(
            methods
                .iter()
                .map(|value| value.as_str().expect("auth method should be a string"))
                .collect::<Vec<_>>(),
            vec!["github"]
        );
    }

    #[test]
    fn persist_install_outputs_direct_restores_settings_and_vault_on_secret_failure() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path());
        let settings_path = dir.path().join("settings.toml");
        std::fs::write(&settings_path, "_version = 1\n[server]\n").unwrap();
        let vault_path = storage.secrets_path();
        let mut vault = Vault::load(vault_path.clone()).unwrap();
        vault
            .set(
                "EXISTING_SECRET",
                "keep",
                VaultSecretType::Environment,
                None,
            )
            .unwrap();

        let result = persist_install_outputs_direct(
            dir.path(),
            &[envfile::EnvFileUpdate {
                key:     "SESSION_SECRET".to_string(),
                value:   "session".to_string(),
                comment: None,
            }],
            &[],
            &[VaultSecretWrite {
                name:        "bad-secret-name".to_string(),
                value:       "boom".to_string(),
                secret_type: VaultSecretType::Environment,
                description: None,
            }],
            Some(&PendingSettingsWrite {
                path:              &settings_path,
                contents:          "_version = 1\n[server]\nfoo = \"bar\"\n",
                previous_contents: Some("_version = 1\n[server]\n"),
            }),
        );

        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&settings_path).unwrap(),
            "_version = 1\n[server]\n"
        );

        let restored = Vault::load(vault_path).unwrap();
        assert_eq!(restored.get("EXISTING_SECRET"), Some("keep"));
        assert_eq!(restored.get("bad-secret-name"), None);

        let server_env = envfile::read_env_file(&storage.runtime_directory().env_path()).unwrap();
        assert_eq!(
            server_env.get("SESSION_SECRET").map(String::as_str),
            Some("session")
        );
    }

    #[test]
    fn merge_server_settings_keeps_tcp_bind_separate_from_public_web_url() {
        use fabro_types::settings::server::ServerListenSettings;

        let mut doc = toml::Value::Table(toml::Table::default());
        merge_server_settings(
            &mut doc,
            "https://fabro.example.com",
            &InstallListenConfig::Tcp("0.0.0.0:32276".to_string()),
        )
        .unwrap();

        let resolved = ServerSettingsBuilder::from_toml(
            &toml::to_string_pretty(&doc).expect("settings should serialize"),
        )
        .expect("settings should resolve")
        .server;
        match resolved.listen {
            ServerListenSettings::Tcp { address, .. } => {
                assert_eq!(address.to_string(), "0.0.0.0:32276");
            }
            ServerListenSettings::Unix { .. } => {
                panic!("expected tcp listen settings");
            }
        }
    }

    #[test]
    fn write_object_store_settings_keeps_local_defaults_and_removes_managed_keys() {
        let mut doc = toml::Value::Table(toml::Table::default());
        let plan = write_object_store_settings(&mut doc, &InstallObjectStoreSelection::Local)
            .expect("local object store selection should succeed");

        assert!(
            doc.get("server")
                .and_then(toml::Value::as_table)
                .and_then(|server| server.get("artifacts"))
                .is_none()
        );
        assert!(plan.writes.is_empty());
        assert_eq!(plan.removals.len(), 2);
    }

    #[test]
    fn write_object_store_settings_configures_s3_runtime_credentials() {
        let mut doc = toml::Value::Table(toml::Table::default());
        let plan = write_object_store_settings(&mut doc, &InstallObjectStoreSelection::S3 {
            bucket:            "fabro-data".to_string(),
            region:            "us-east-1".to_string(),
            credential_mode:   InstallObjectStoreCredentialMode::Runtime,
            access_key_id:     None,
            secret_access_key: None,
        })
        .expect("runtime-credential object store selection should succeed");

        let server = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .expect("server table should exist");
        assert_eq!(
            server
                .get("artifacts")
                .and_then(toml::Value::as_table)
                .and_then(|artifacts| artifacts.get("prefix"))
                .and_then(toml::Value::as_str),
            Some("artifacts")
        );
        assert_eq!(
            server
                .get("slatedb")
                .and_then(toml::Value::as_table)
                .and_then(|slatedb| slatedb.get("prefix"))
                .and_then(toml::Value::as_str),
            Some("slatedb")
        );
        assert!(plan.writes.is_empty());
    }

    #[test]
    fn write_object_store_settings_configures_s3_manual_credentials() {
        let mut doc = toml::Value::Table(toml::Table::default());
        let plan = write_object_store_settings(&mut doc, &InstallObjectStoreSelection::S3 {
            bucket:            "fabro-data".to_string(),
            region:            "us-east-1".to_string(),
            credential_mode:   InstallObjectStoreCredentialMode::AccessKey,
            access_key_id:     Some("AKIA_TEST".to_string()),
            secret_access_key: Some("secret-test".to_string()),
        })
        .expect("manual-credential object store selection should succeed");

        assert_eq!(plan.writes.len(), 2);
        assert!(
            plan.writes
                .iter()
                .all(|write| write.comment.as_deref() == Some(OBJECT_STORE_MANAGED_COMMENT))
        );
        assert_eq!(
            plan.writes
                .iter()
                .find(|write| write.key == OBJECT_STORE_ACCESS_KEY_ID_ENV)
                .map(|write| write.value.as_str()),
            Some("AKIA_TEST")
        );
        assert_eq!(
            plan.writes
                .iter()
                .find(|write| write.key == OBJECT_STORE_SECRET_ACCESS_KEY_ENV)
                .map(|write| write.value.as_str()),
            Some("secret-test")
        );
    }

    #[test]
    fn persist_install_outputs_direct_only_removes_marked_object_store_keys() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path());
        let env_path = storage.runtime_directory().env_path();
        std::fs::create_dir_all(env_path.parent().unwrap()).unwrap();
        std::fs::write(
            &env_path,
            format!(
                "{OBJECT_STORE_ACCESS_KEY_ID_ENV}=operator-access\n# {OBJECT_STORE_MANAGED_COMMENT}\n{OBJECT_STORE_ACCESS_KEY_ID_ENV}=managed-access\n{OBJECT_STORE_SECRET_ACCESS_KEY_ENV}=operator-secret\nKEEP_ME=1\n"
            ),
        )
        .unwrap();

        persist_install_outputs_direct(
            dir.path(),
            &[],
            &[envfile::EnvFileRemoval {
                key:     OBJECT_STORE_ACCESS_KEY_ID_ENV.to_string(),
                comment: Some(OBJECT_STORE_MANAGED_COMMENT.to_string()),
            }],
            &[],
            None,
        )
        .expect("env-only persistence should succeed");

        let server_env = envfile::read_env_file(&env_path).unwrap();
        assert_eq!(
            server_env
                .get(OBJECT_STORE_ACCESS_KEY_ID_ENV)
                .map(String::as_str),
            Some("operator-access")
        );
        assert_eq!(
            server_env
                .get(OBJECT_STORE_SECRET_ACCESS_KEY_ENV)
                .map(String::as_str),
            Some("operator-secret")
        );
        assert_eq!(server_env.get("KEEP_ME").map(String::as_str), Some("1"));
    }

    #[cfg(unix)]
    #[test]
    fn persist_install_outputs_direct_writes_private_server_env_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path());
        let env_path = storage.runtime_directory().env_path();

        persist_install_outputs_direct(
            dir.path(),
            &[envfile::EnvFileUpdate {
                key:     "SESSION_SECRET".to_string(),
                value:   "first".to_string(),
                comment: None,
            }],
            &[],
            &[],
            None,
        )
        .expect("initial env write should succeed");
        let create_mode = std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(create_mode, 0o600);

        persist_install_outputs_direct(
            dir.path(),
            &[envfile::EnvFileUpdate {
                key:     "SESSION_SECRET".to_string(),
                value:   "second".to_string(),
                comment: None,
            }],
            &[],
            &[],
            None,
        )
        .expect("rewrite env write should succeed");
        let update_mode = std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(update_mode, 0o600);
    }
}
