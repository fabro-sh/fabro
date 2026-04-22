#![expect(
    clippy::disallowed_methods,
    reason = "fabro-install: sync CLI install/uninstall bookkeeping; not on a Tokio hot path"
)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_config::{Storage, envfile};
use fabro_vault::{SecretType as VaultSecretType, Vault};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2A, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const ED25519_PUBLIC_KEY_LEN: usize = 32;

pub struct PendingSettingsWrite<'a> {
    pub path:              &'a Path,
    pub contents:          &'a str,
    pub previous_contents: Option<&'a str>,
}

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

fn pem_encode(label: &str, bytes: &[u8]) -> String {
    let body = BASE64_STANDARD.encode(bytes);
    let mut pem = String::new();
    pem.push_str("-----BEGIN ");
    pem.push_str(label);
    pem.push_str("-----\n");
    for chunk in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 output should be valid UTF-8"));
        pem.push('\n');
    }
    pem.push_str("-----END ");
    pem.push_str(label);
    pem.push_str("-----\n");
    pem
}

fn ed25519_public_key_spki(public_key: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(
        public_key.len() == ED25519_PUBLIC_KEY_LEN,
        "generated Ed25519 public key had unexpected length"
    );

    let mut spki = Vec::with_capacity(ED25519_SPKI_PREFIX.len() + public_key.len());
    spki.extend_from_slice(&ED25519_SPKI_PREFIX);
    spki.extend_from_slice(public_key);
    Ok(spki)
}

pub fn generate_jwt_keypair() -> Result<(String, String)> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| anyhow::anyhow!("failed to generate Ed25519 keypair"))?;
    let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| anyhow::anyhow!("failed to parse generated Ed25519 keypair"))?;
    let public_der = ed25519_public_key_spki(keypair.public_key().as_ref())?;

    Ok((
        pem_encode("PRIVATE KEY", pkcs8.as_ref()),
        pem_encode("PUBLIC KEY", &public_der),
    ))
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

fn persist_server_env_secrets(storage_dir: &Path, secrets: &[(String, String)]) -> Result<()> {
    if secrets.is_empty() {
        return Ok(());
    }

    let env_path = Storage::new(storage_dir).runtime_state().env_path();
    envfile::merge_env_file(&env_path, secrets.iter().cloned())
        .with_context(|| format!("merging server env secrets into {}", env_path.display()))?;
    Ok(())
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
    server_env_secrets: &[(String, String)],
    vault_secrets: &[VaultSecretWrite],
    settings_write: Option<&PendingSettingsWrite<'_>>,
) -> Result<()> {
    persist_server_env_secrets(storage_dir, server_env_secrets)?;

    if let Some(write) = settings_write {
        if let Some(parent) = write.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating settings directory {}", parent.display()))?;
        }
        std::fs::write(write.path, write.contents)
            .with_context(|| format!("writing settings file {}", write.path.display()))?;
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
        return Err(error);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use fabro_config::{Storage, envfile};
    use fabro_vault::{SecretType as VaultSecretType, Vault};

    use super::{
        InstallListenConfig, PendingSettingsWrite, VaultSecretWrite, default_web_url,
        merge_server_settings, persist_install_outputs_direct, write_github_app_settings,
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
        use fabro_types::settings::{ServerAuthMethod, SettingsLayer};

        let toml_str = format_config_toml();
        let cfg: SettingsLayer = fabro_config::parse_settings_layer(&toml_str).unwrap();
        let auth = cfg
            .server
            .as_ref()
            .and_then(|s| s.auth.as_ref())
            .expect("server.auth should be set");
        assert_eq!(auth.methods, Some(vec![ServerAuthMethod::DevToken]));
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
            &[("SESSION_SECRET".to_string(), "session".to_string())],
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

        let server_env = envfile::read_env_file(&storage.runtime_state().env_path()).unwrap();
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

        let settings = fabro_config::parse_settings_layer(
            &toml::to_string_pretty(&doc).expect("settings should serialize"),
        )
        .expect("settings should parse");
        let resolved =
            fabro_config::resolve_server_from_file(&settings).expect("settings should resolve");
        match resolved.listen {
            ServerListenSettings::Tcp { address, .. } => {
                assert_eq!(address.to_string(), "0.0.0.0:32276");
            }
            ServerListenSettings::Unix { .. } => {
                panic!("expected tcp listen settings");
            }
        }
    }
}
