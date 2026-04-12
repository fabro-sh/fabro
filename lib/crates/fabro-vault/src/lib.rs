use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretType {
    #[default]
    Environment,
    File,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretEntry {
    pub value:       String,
    #[serde(rename = "type", default)]
    pub secret_type: SecretType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at:  String,
    pub updated_at:  String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretMetadata {
    pub name:        String,
    #[serde(rename = "type")]
    pub secret_type: SecretType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at:  String,
    pub updated_at:  String,
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
            Err(err) => return Err(err.into()),
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

        let now = chrono::Utc::now().to_rfc3339();
        let (created_at, description) = self.entries.get(name).map_or_else(
            || (now.clone(), description.map(str::to_string)),
            |entry| {
                (
                    entry.created_at.clone(),
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
            created_at: created_at.clone(),
            updated_at: now.clone(),
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
                created_at:  entry.created_at.clone(),
                updated_at:  entry.updated_at.clone(),
            })
            .collect::<Vec<_>>();
        data.sort_by(|a, b| a.name.cmp(&b.name));
        data
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|entry| entry.value.as_str())
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.secret_type == SecretType::Environment)
            .map(|(name, entry)| (name.clone(), entry.value.clone()))
            .collect()
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
            SecretType::Environment => Self::validate_env_name(name),
            SecretType::File => Self::validate_file_name(name),
        }
    }

    fn validate_env_name(name: &str) -> Result<(), Error> {
        let mut chars = name.chars();
        match chars.next() {
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
            _ => return Err(Error::InvalidName(name.to_string())),
        }

        if chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            Ok(())
        } else {
            Err(Error::InvalidName(name.to_string()))
        }
    }

    fn validate_file_name(name: &str) -> Result<(), Error> {
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
        std::fs::create_dir_all(&parent)?;

        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("secrets.json");
        let tmp_path = parent.join(format!(".{file_name}.tmp-{}", ulid::Ulid::new()));
        let json = serde_json::to_vec_pretty(&self.entries)?;
        std::fs::write(&tmp_path, json)?;
        set_private_permissions(&tmp_path)?;
        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
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
            .set("OPENAI_API_KEY", "secret", SecretType::Environment, None)
            .unwrap();

        assert_eq!(meta.name, "OPENAI_API_KEY");
        assert_eq!(meta.secret_type, SecretType::Environment);
        assert_eq!(store.get("OPENAI_API_KEY"), Some("secret"));
        assert!(path.exists());
    }

    #[test]
    fn set_updates_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = Vault::load(path).unwrap();

        store
            .set("OPENAI_API_KEY", "first", SecretType::Environment, None)
            .unwrap();
        store
            .set("OPENAI_API_KEY", "second", SecretType::Environment, None)
            .unwrap();

        assert_eq!(store.get("OPENAI_API_KEY"), Some("second"));
    }

    #[test]
    fn remove_deletes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = Vault::load(path.clone()).unwrap();
        store
            .set("OPENAI_API_KEY", "secret", SecretType::Environment, None)
            .unwrap();

        store.remove("OPENAI_API_KEY").unwrap();

        assert_eq!(store.get("OPENAI_API_KEY"), None);
    }

    #[test]
    fn env_secret_snapshot_excludes_file_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Vault::load(dir.path().join("secrets.json")).unwrap();
        store
            .set("OPENAI_API_KEY", "env", SecretType::Environment, None)
            .unwrap();
        store
            .set("/tmp/key.pem", "pem", SecretType::File, None)
            .unwrap();

        let snapshot = store.snapshot();
        assert_eq!(snapshot.get("OPENAI_API_KEY"), Some(&"env".to_string()));
        assert!(!snapshot.contains_key("/tmp/key.pem"));
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
}
