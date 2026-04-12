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

    pub fn set(
        &mut self,
        name: &str,
        value: &str,
        secret_type: SecretType,
        description: Option<&str>,
    ) -> Result<SecretMetadata, SecretStoreError> {
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

    pub fn remove(&mut self, name: &str) -> Result<(), SecretStoreError> {
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

    pub fn validate_name(name: &str, secret_type: SecretType) -> Result<(), SecretStoreError> {
        match secret_type {
            SecretType::Environment => Self::validate_env_name(name),
            SecretType::File => Self::validate_file_name(name),
        }
    }

    fn validate_env_name(name: &str) -> Result<(), SecretStoreError> {
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

    fn validate_file_name(name: &str) -> Result<(), SecretStoreError> {
        if !name.starts_with('/') || name.ends_with('/') || name.contains('\0') {
            return Err(SecretStoreError::InvalidName(name.to_string()));
        }

        let path = Path::new(name);
        if !path.is_absolute() {
            return Err(SecretStoreError::InvalidName(name.to_string()));
        }

        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(SecretStoreError::InvalidName(name.to_string()));
        }

        Ok(())
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

        let meta = store
            .set("OPENAI_API_KEY", "secret", SecretType::Environment, None)
            .unwrap();

        assert_eq!(meta.name, "OPENAI_API_KEY");
        assert_eq!(meta.secret_type, SecretType::Environment);
        assert_eq!(meta.description, None);
        assert_eq!(store.get("OPENAI_API_KEY"), Some("secret"));
        assert!(path.exists());
    }

    #[test]
    fn set_existing_key_preserves_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = SecretStore::load(path).unwrap();

        let first = store
            .set("OPENAI_API_KEY", "first", SecretType::Environment, None)
            .unwrap();
        let second = store
            .set("OPENAI_API_KEY", "second", SecretType::Environment, None)
            .unwrap();

        assert_eq!(first.created_at, second.created_at);
        assert_eq!(store.get("OPENAI_API_KEY"), Some("second"));
    }

    #[test]
    fn remove_deletes_entry_and_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store
            .set("OPENAI_API_KEY", "secret", SecretType::Environment, None)
            .unwrap();

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
        store
            .set("Z_KEY", "z", SecretType::Environment, None)
            .unwrap();
        store
            .set("A_KEY", "a", SecretType::Environment, None)
            .unwrap();

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
        let error = store
            .set("NOT-VALID", "secret", SecretType::Environment, None)
            .unwrap_err();
        assert_eq!(error.to_string(), "invalid secret name: NOT-VALID");
    }

    #[test]
    fn set_file_secret_stores_type() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = SecretStore::load(path.clone()).unwrap();

        let meta = store
            .set("/root/.ssh/id_rsa", "secret", SecretType::File, None)
            .unwrap();

        assert_eq!(meta.name, "/root/.ssh/id_rsa");
        assert_eq!(meta.secret_type, SecretType::File);

        let reloaded = SecretStore::load(path).unwrap();
        let listed = reloaded.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].secret_type, SecretType::File);
    }

    #[test]
    fn set_file_secret_validates_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();

        let error = store
            .set("relative/path", "secret", SecretType::File, None)
            .unwrap_err();

        assert_eq!(error.to_string(), "invalid secret name: relative/path");
    }

    #[test]
    fn set_file_secret_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();

        let error = store
            .set("/root/../id_rsa", "secret", SecretType::File, None)
            .unwrap_err();

        assert_eq!(error.to_string(), "invalid secret name: /root/../id_rsa");
    }

    #[test]
    fn set_env_secret_rejects_path_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();

        let error = store
            .set("/foo/bar", "secret", SecretType::Environment, None)
            .unwrap_err();

        assert_eq!(error.to_string(), "invalid secret name: /foo/bar");
    }

    #[test]
    fn snapshot_excludes_file_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();
        store
            .set("OPENAI_API_KEY", "env", SecretType::Environment, None)
            .unwrap();
        store
            .set("/tmp/test.pem", "file", SecretType::File, None)
            .unwrap();

        let snapshot = store.snapshot();

        assert_eq!(snapshot.get("OPENAI_API_KEY"), Some(&"env".to_string()));
        assert!(!snapshot.contains_key("/tmp/test.pem"));
    }

    #[test]
    fn snapshot_includes_only_env_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();
        store
            .set("ANTHROPIC_API_KEY", "a", SecretType::Environment, None)
            .unwrap();
        store
            .set("OPENAI_API_KEY", "b", SecretType::Environment, None)
            .unwrap();

        let snapshot = store.snapshot();

        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.get("ANTHROPIC_API_KEY"), Some(&"a".to_string()));
        assert_eq!(snapshot.get("OPENAI_API_KEY"), Some(&"b".to_string()));
    }

    #[test]
    fn file_secrets_returns_only_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();
        store
            .set("OPENAI_API_KEY", "env", SecretType::Environment, None)
            .unwrap();
        store
            .set("/tmp/test.pem", "file", SecretType::File, None)
            .unwrap();

        let files = store.file_secrets();

        assert_eq!(files, vec![(
            "/tmp/test.pem".to_string(),
            "file".to_string()
        )]);
    }

    #[test]
    fn description_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut store = SecretStore::load(path.clone()).unwrap();

        let meta = store
            .set(
                "/tmp/test.pem",
                "file",
                SecretType::File,
                Some("Test certificate"),
            )
            .unwrap();

        assert_eq!(meta.description.as_deref(), Some("Test certificate"));

        let reloaded = SecretStore::load(path).unwrap();
        let listed = reloaded.list();
        assert_eq!(listed[0].description.as_deref(), Some("Test certificate"));
    }

    #[test]
    fn legacy_json_defaults_to_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(
            &path,
            r#"{
  "OPENAI_API_KEY": {
    "value": "secret",
    "created_at": "2026-04-12T00:00:00Z",
    "updated_at": "2026-04-12T00:00:00Z"
  }
}"#,
        )
        .unwrap();

        let store = SecretStore::load(path).unwrap();
        let listed = store.list();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].secret_type, SecretType::Environment);
        assert_eq!(listed[0].description, None);
    }

    #[test]
    fn remove_allows_file_path_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::load(dir.path().join("secrets.json")).unwrap();
        store
            .set("/tmp/test.pem", "file", SecretType::File, None)
            .unwrap();

        store.remove("/tmp/test.pem").unwrap();

        assert!(store.list().is_empty());
    }
}
