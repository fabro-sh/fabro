use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretEntry {
    pub value:      String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretMetadata {
    pub name:       String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug)]
pub enum SecretStoreError {
    InvalidName(String),
    NotFound(String),
    Io(std::io::Error),
    Serde(serde_json::Error),
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(f, "invalid secret name: {name}"),
            Self::NotFound(name) => write!(f, "secret not found: {name}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Serde(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SecretStoreError {}

impl From<std::io::Error> for SecretStoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for SecretStoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}

#[derive(Debug)]
pub struct SecretStore {
    path:    PathBuf,
    entries: HashMap<String, SecretEntry>,
}

impl SecretStore {
    pub fn load(path: PathBuf) -> Result<Self, SecretStoreError> {
        let entries = match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(err) => return Err(err.into()),
        };

        Ok(Self { path, entries })
    }

    pub fn set(&mut self, name: &str, value: &str) -> Result<SecretMetadata, SecretStoreError> {
        Self::validate_name(name)?;

        let now = chrono::Utc::now().to_rfc3339();
        let created_at = self
            .entries
            .get(name)
            .map_or_else(|| now.clone(), |entry| entry.created_at.clone());
        let entry = SecretEntry {
            value:      value.to_string(),
            created_at: created_at.clone(),
            updated_at: now.clone(),
        };
        self.entries.insert(name.to_string(), entry);
        self.write_atomic()?;

        Ok(SecretMetadata {
            name: name.to_string(),
            created_at,
            updated_at: now,
        })
    }

    pub fn remove(&mut self, name: &str) -> Result<(), SecretStoreError> {
        Self::validate_name(name)?;
        if self.entries.remove(name).is_none() {
            return Err(SecretStoreError::NotFound(name.to_string()));
        }
        self.write_atomic()?;
        Ok(())
    }

    pub fn list(&self) -> Vec<SecretMetadata> {
        let mut data = self
            .entries
            .iter()
            .map(|(name, entry)| SecretMetadata {
                name:       name.clone(),
                created_at: entry.created_at.clone(),
                updated_at: entry.updated_at.clone(),
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
            .map(|(name, entry)| (name.clone(), entry.value.clone()))
            .collect()
    }

    pub fn validate_name(name: &str) -> Result<(), SecretStoreError> {
        let mut chars = name.chars();
        match chars.next() {
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
            _ => return Err(SecretStoreError::InvalidName(name.to_string())),
        }

        if chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            Ok(())
        } else {
            Err(SecretStoreError::InvalidName(name.to_string()))
        }
    }

    fn write_atomic(&self) -> Result<(), SecretStoreError> {
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
fn set_private_permissions(path: &Path) -> Result<(), SecretStoreError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), SecretStoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::load(dir.path().join("secrets.json")).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn set_creates_entry_and_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = SecretStore::load(path.clone()).unwrap();

        let meta = store.set("OPENAI_API_KEY", "secret").unwrap();

        assert_eq!(meta.name, "OPENAI_API_KEY");
        assert_eq!(store.get("OPENAI_API_KEY"), Some("secret"));
        assert!(path.exists());
    }

    #[test]
    fn set_existing_key_preserves_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = SecretStore::load(path).unwrap();

        let first = store.set("OPENAI_API_KEY", "first").unwrap();
        let second = store.set("OPENAI_API_KEY", "second").unwrap();

        assert_eq!(first.created_at, second.created_at);
        assert_eq!(store.get("OPENAI_API_KEY"), Some("second"));
    }

    #[test]
    fn remove_deletes_entry_and_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store.set("OPENAI_API_KEY", "secret").unwrap();

        store.remove("OPENAI_API_KEY").unwrap();

        assert_eq!(store.get("OPENAI_API_KEY"), None);
        let written = std::fs::read_to_string(path).unwrap();
        assert_eq!(written.trim(), "{}");
    }

    #[test]
    fn remove_missing_key_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();
        let error = store.remove("MISSING").unwrap_err();
        assert_eq!(error.to_string(), "secret not found: MISSING");
    }

    #[test]
    fn list_returns_sorted_metadata_without_values() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();
        store.set("Z_KEY", "z").unwrap();
        store.set("A_KEY", "a").unwrap();

        let listed = store.list();

        assert_eq!(
            listed
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A_KEY", "Z_KEY"]
        );
    }

    #[test]
    fn invalid_names_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();
        let error = store.set("NOT-VALID", "secret").unwrap_err();
        assert_eq!(error.to_string(), "invalid secret name: NOT-VALID");
    }
}
