#![expect(
    clippy::disallowed_methods,
    reason = "fabro-vault: sync secret-file storage; not used on a Tokio hot path"
)]

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::{fmt, io};

use chrono::{DateTime, Utc};
use fabro_static::EnvVars;
pub use fabro_types::SecretType;
use fabro_types::{SecretMetadata, is_env_style_name};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretEntry {
    pub value:       String,
    #[serde(rename = "type", default)]
    pub secret_type: SecretType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at:  DateTime<Utc>,
    pub updated_at:  DateTime<Utc>,
}

#[derive(Debug)]
pub enum Error {
    InvalidName(String),
    NotFound(String),
    Io(std::io::Error),
    Serde(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(f, "invalid secret name: {name}"),
            Self::NotFound(name) => write!(f, "secret not found: {name}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Serde(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}

#[derive(Debug)]
pub struct Vault {
    path:    PathBuf,
    entries: HashMap<String, SecretEntry>,
}

impl Vault {
    pub fn load(path: PathBuf) -> Result<Self, Error> {
        let entries = match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(err) => return Err(io_context("read vault", &path, &err).into()),
        };

        Ok(Self { path, entries })
    }

    pub fn set(
        &mut self,
        name: &str,
        value: &str,
        secret_type: SecretType,
        description: Option<&str>,
    ) -> Result<SecretMetadata, Error> {
        Self::validate_name(name, secret_type)?;

        let now = Utc::now();
        let (created_at, description) = self.entries.get(name).map_or_else(
            || (now, description.map(str::to_string)),
            |entry| {
                (
                    entry.created_at,
                    description
                        .map(str::to_string)
                        .or_else(|| entry.description.clone()),
                )
            },
        );
        let entry = SecretEntry {
            value: value.to_string(),
            secret_type,
            description: description.clone(),
            created_at,
            updated_at: now,
        };
        self.entries.insert(name.to_string(), entry);
        self.write_atomic()?;

        Ok(SecretMetadata {
            name: name.to_string(),
            secret_type,
            description,
            created_at,
            updated_at: now,
        })
    }

    pub fn remove(&mut self, name: &str) -> Result<(), Error> {
        if self.entries.remove(name).is_none() {
            return Err(Error::NotFound(name.to_string()));
        }
        self.write_atomic()?;
        Ok(())
    }

    pub fn list(&self) -> Vec<SecretMetadata> {
        let mut data = self
            .entries
            .iter()
            .map(|(name, entry)| SecretMetadata {
                name:        name.clone(),
                secret_type: entry.secret_type,
                description: entry.description.clone(),
                created_at:  entry.created_at,
                updated_at:  entry.updated_at,
            })
            .collect::<Vec<_>>();
        data.sort_by(|a, b| a.name.cmp(&b.name));
        data
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|entry| entry.value.as_str())
    }

    pub fn get_entry(&self, name: &str) -> Option<&SecretEntry> {
        self.entries.get(name)
    }

    pub fn file_secrets(&self) -> Vec<(String, String)> {
        let mut data = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.secret_type == SecretType::File)
            .map(|(name, entry)| (name.clone(), entry.value.clone()))
            .collect::<Vec<_>>();
        data.sort_by(|a, b| a.0.cmp(&b.0));
        data
    }

    pub fn validate_name(name: &str, secret_type: SecretType) -> Result<(), Error> {
        match secret_type {
            SecretType::Token | SecretType::Oauth => Self::validate_env_name(name),
            SecretType::File => Self::validate_file_name(name),
        }
    }

    fn validate_env_name(name: &str) -> Result<(), Error> {
        if is_env_style_name(name) {
            Ok(())
        } else {
            Err(Error::InvalidName(name.to_string()))
        }
    }

    fn validate_file_name(name: &str) -> Result<(), Error> {
        if name == EnvVars::GITHUB_APP_PRIVATE_KEY {
            return Ok(());
        }

        if !name.starts_with('/') || name.ends_with('/') || name.contains('\0') {
            return Err(Error::InvalidName(name.to_string()));
        }

        let path = Path::new(name);
        if !path.is_absolute() {
            return Err(Error::InvalidName(name.to_string()));
        }

        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(Error::InvalidName(name.to_string()));
        }

        Ok(())
    }

    fn write_atomic(&self) -> Result<(), Error> {
        let parent = self
            .path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        std::fs::create_dir_all(&parent)
            .map_err(|err| io_context("create vault directory", &parent, &err))?;

        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("secrets.json");
        let tmp_path = parent.join(format!(".{file_name}.tmp-{}", ulid::Ulid::new()));
        let json = serde_json::to_vec_pretty(&self.entries)?;
        std::fs::write(&tmp_path, json)
            .map_err(|err| io_context("write vault temp file", &tmp_path, &err))?;
        set_private_permissions(&tmp_path)?;
        std::fs::rename(&tmp_path, &self.path).map_err(|err| {
            io_context(
                &format!("rename vault temp file to {}", self.path.display()),
                &tmp_path,
                &err,
            )
        })?;
        Ok(())
    }
}

/// Wrap an `io::Error` with a human-readable verb and path so downstream
/// reporting shows which operation failed on which file.
fn io_context(op: &str, path: &Path, source: &io::Error) -> io::Error {
    io::Error::new(source.kind(), format!("{op} {}: {source}", path.display()))
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| io_context("set permissions on", path, &err))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Vault::load(dir.path().join("secrets.json")).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn set_creates_entry_and_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = Vault::load(path.clone()).unwrap();

        let meta = store
            .set("OPENAI_API_KEY", "secret", SecretType::Token, None)
            .unwrap();

        assert_eq!(meta.name, "OPENAI_API_KEY");
        assert_eq!(meta.secret_type, SecretType::Token);
        assert_eq!(store.get("OPENAI_API_KEY"), Some("secret"));
        assert!(path.exists());
    }

    #[test]
    fn set_updates_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = Vault::load(path).unwrap();

        store
            .set("OPENAI_API_KEY", "first", SecretType::Token, None)
            .unwrap();
        store
            .set("OPENAI_API_KEY", "second", SecretType::Token, None)
            .unwrap();

        assert_eq!(store.get("OPENAI_API_KEY"), Some("second"));
    }

    #[test]
    fn remove_deletes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = Vault::load(path.clone()).unwrap();
        store
            .set("OPENAI_API_KEY", "secret", SecretType::Token, None)
            .unwrap();

        store.remove("OPENAI_API_KEY").unwrap();

        assert_eq!(store.get("OPENAI_API_KEY"), None);
    }

    #[test]
    fn file_secrets_excludes_token_and_oauth_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Vault::load(dir.path().join("secrets.json")).unwrap();
        store
            .set("OPENAI_API_KEY", "token", SecretType::Token, None)
            .unwrap();
        store
            .set("OPENAI_CODEX", "oauth-json", SecretType::Oauth, None)
            .unwrap();
        store
            .set("/tmp/key.pem", "pem", SecretType::File, None)
            .unwrap();

        assert_eq!(store.file_secrets(), vec![(
            "/tmp/key.pem".to_string(),
            "pem".to_string()
        )]);
    }

    #[test]
    fn file_secret_listing_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = Vault::load(path.clone()).unwrap();
        store
            .set("/tmp/key.pem", "pem", SecretType::File, None)
            .unwrap();

        let reloaded = Vault::load(path).unwrap();
        assert_eq!(reloaded.file_secrets(), vec![(
            "/tmp/key.pem".to_string(),
            "pem".to_string()
        )]);
    }

    #[test]
    fn github_app_private_key_may_be_stored_as_file_secret() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Vault::load(dir.path().join("secrets.json")).unwrap();

        store
            .set(
                EnvVars::GITHUB_APP_PRIVATE_KEY,
                "base64-pem",
                SecretType::File,
                None,
            )
            .unwrap();

        assert_eq!(store.file_secrets(), vec![(
            EnvVars::GITHUB_APP_PRIVATE_KEY.to_string(),
            "base64-pem".to_string()
        )]);
    }

    #[test]
    fn list_includes_schema_typed_entries_loaded_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "OPENAI_API_KEY": {
                    "value": "token",
                    "type": "token",
                    "created_at": "2026-04-12T00:00:00Z",
                    "updated_at": "2026-04-12T00:00:00Z"
                },
                "OPENAI_CODEX": {
                    "value": "{\"tokens\":{\"access_token\":\"access\",\"refresh_token\":\"refresh\",\"expires_at\":\"2026-04-12T01:00:00Z\"},\"config\":{\"auth_url\":\"https://auth.openai.com\",\"token_url\":\"https://auth.openai.com/oauth/token\",\"client_id\":\"client\",\"scopes\":[\"openid\"],\"redirect_uri\":null,\"use_pkce\":true}}",
                    "type": "oauth",
                    "created_at": "2026-04-12T00:00:00Z",
                    "updated_at": "2026-04-12T00:00:00Z"
                }
            })
            .to_string(),
        )
        .unwrap();

        let store = Vault::load(path).unwrap();

        let list = store.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "OPENAI_API_KEY");
        assert_eq!(list[0].secret_type, SecretType::Token);
        assert_eq!(list[1].name, "OPENAI_CODEX");
        assert_eq!(list[1].secret_type, SecretType::Oauth);
        assert_eq!(store.get("OPENAI_API_KEY"), Some("token"));
        assert!(store.get("OPENAI_CODEX").is_some());
    }

    #[test]
    fn get_entry_returns_full_secret_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Vault::load(dir.path().join("secrets.json")).unwrap();
        store
            .set(
                "OPENAI_CODEX",
                "oauth-json",
                SecretType::Oauth,
                Some("saved auth"),
            )
            .unwrap();

        let entry = store.get_entry("OPENAI_CODEX").unwrap();

        assert_eq!(entry.value, "oauth-json");
        assert_eq!(entry.secret_type, SecretType::Oauth);
        assert_eq!(entry.description.as_deref(), Some("saved auth"));
    }

    #[test]
    fn get_entry_returns_token_entries_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Vault::load(dir.path().join("secrets.json")).unwrap();
        store
            .set("OPENAI_API_KEY", "token", SecretType::Token, None)
            .unwrap();

        let entry = store.get_entry("OPENAI_API_KEY").unwrap();
        assert_eq!(entry.value, "token");
        assert_eq!(entry.secret_type, SecretType::Token);
    }
}
