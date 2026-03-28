use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use fabro_agent::Sandbox;

use crate::error::{FabroError, Result};

/// Threshold above which artifacts are stored on disk instead of in memory (100KB).
const FILE_BACKING_THRESHOLD: usize = 100 * 1024;

/// Metadata about a stored artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub id: String,
    pub name: String,
    pub size_bytes: usize,
    pub stored_at: DateTime<Utc>,
    pub is_file_backed: bool,
    pub file_path: Option<PathBuf>,
}

/// Storage for artifacts, either held in memory or backed by files on disk.
enum StoredData {
    InMemory(Value),
    FileBacked(PathBuf),
}

/// Named, typed storage for large stage outputs.
pub struct ArtifactStore {
    values_dir: Option<PathBuf>,
    artifacts: RwLock<HashMap<String, (ArtifactInfo, StoredData)>>,
}

impl std::fmt::Debug for ArtifactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactStore")
            .field("values_dir", &self.values_dir)
            .finish_non_exhaustive()
    }
}

impl ArtifactStore {
    #[must_use]
    pub fn new(values_dir: Option<PathBuf>) -> Self {
        Self {
            values_dir,
            artifacts: RwLock::new(HashMap::new()),
        }
    }

    /// Store an artifact. Large artifacts with a configured `values_dir` are written to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails or the file cannot be written.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    pub fn store(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
        data: Value,
    ) -> Result<ArtifactInfo> {
        let id = id.into();
        let name = name.into();
        let serialized = serde_json::to_string(&data)
            .map_err(|e| FabroError::engine(format!("artifact serialize failed: {e}")))?;
        let size_bytes = serialized.len();

        let is_file_backed = size_bytes > FILE_BACKING_THRESHOLD && self.values_dir.is_some();

        let (stored, file_path) = if is_file_backed {
            let values_dir = self.values_dir.as_ref().expect("values_dir checked above");
            std::fs::create_dir_all(values_dir)?;
            let path = values_dir.join(format!("{id}.json"));
            std::fs::write(&path, &serialized)?;
            (StoredData::FileBacked(path.clone()), Some(path))
        } else {
            (StoredData::InMemory(data), None)
        };

        let info = ArtifactInfo {
            id: id.clone(),
            name,
            size_bytes,
            stored_at: Utc::now(),
            is_file_backed,
            file_path,
        };

        self.artifacts
            .write()
            .expect("artifact lock poisoned")
            .insert(id, (info.clone(), stored));

        Ok(info)
    }

    /// Retrieve an artifact's data by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the artifact is not found or cannot be read from disk.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    pub fn retrieve(&self, id: &str) -> Result<Value> {
        let guard = self.artifacts.read().expect("artifact lock poisoned");
        let (_, stored) = guard
            .get(id)
            .ok_or_else(|| FabroError::engine(format!("artifact not found: {id}")))?;

        match stored {
            StoredData::InMemory(v) => Ok(v.clone()),
            StoredData::FileBacked(path) => {
                let path = path.clone();
                drop(guard);
                let data = std::fs::read_to_string(&path).map_err(|e| {
                    FabroError::engine(format!("failed to read file-backed artifact {id}: {e}"))
                })?;
                serde_json::from_str(&data).map_err(|e| {
                    FabroError::engine(format!(
                        "failed to deserialize file-backed artifact {id}: {e}"
                    ))
                })
            }
        }
    }

    /// Check if an artifact exists.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    pub fn has(&self, id: &str) -> bool {
        self.artifacts
            .read()
            .expect("artifact lock poisoned")
            .contains_key(id)
    }

    /// List all artifact metadata.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    #[must_use]
    pub fn list(&self) -> Vec<ArtifactInfo> {
        self.artifacts
            .read()
            .expect("artifact lock poisoned")
            .values()
            .map(|(info, _)| info.clone())
            .collect()
    }

    /// Remove an artifact by ID. Also deletes file-backed data from disk.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    pub fn remove(&self, id: &str) {
        let mut guard = self.artifacts.write().expect("artifact lock poisoned");
        if let Some((_, StoredData::FileBacked(path))) = guard.remove(id) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Returns the configured values directory.
    /// Returns `None` if no file-backed storage is configured.
    #[must_use]
    pub fn values_dir(&self) -> Option<PathBuf> {
        self.values_dir.clone()
    }

    /// Remove all artifacts. Also deletes file-backed data from disk.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    pub fn clear(&self) {
        let mut guard = self.artifacts.write().expect("artifact lock poisoned");
        for (_, stored) in guard.values() {
            if let StoredData::FileBacked(path) = stored {
                let _ = std::fs::remove_file(path);
            }
        }
        guard.clear();
    }
}

/// Prefix used to identify artifact pointer strings in context values.
const ARTIFACT_POINTER_PREFIX: &str = "file://";

/// Offload context values exceeding the file-backing threshold into the artifact store.
///
/// For each entry in `updates` whose serialized JSON exceeds `FILE_BACKING_THRESHOLD`,
/// the value is stored in `store` and replaced with a `"file://{path}"` pointer.
/// Small values are left untouched.
///
/// # Errors
///
/// Returns an error if storing an artifact fails.
pub fn offload_large_values(
    updates: &mut HashMap<String, Value>,
    store: &ArtifactStore,
) -> Result<()> {
    for (key, value) in updates.iter_mut() {
        let serialized_len = serde_json::to_string(&*value).map(|s| s.len()).unwrap_or(0);
        if serialized_len > FILE_BACKING_THRESHOLD {
            let info = store.store(key, key, value.clone())?;
            if let Some(path) = info.file_path {
                *value = Value::String(format!("{ARTIFACT_POINTER_PREFIX}{}", path.display()));
            }
        }
    }
    Ok(())
}

/// Extract the file path from an artifact pointer value.
///
/// Returns `Some(path)` if the value is a string starting with `"file://"`,
/// `None` otherwise.
#[must_use]
pub fn artifact_path(value: &Value) -> Option<&str> {
    value
        .as_str()
        .and_then(|s| s.strip_prefix(ARTIFACT_POINTER_PREFIX))
}

/// Returns `true` if `path` looks like an artifact pointer path (starts with `"file://"`).
#[must_use]
pub fn is_artifact_pointer(value: &Value) -> bool {
    artifact_path(value).is_some()
}

/// Resolve an artifact pointer to the base name displayed in preamble rendering.
///
/// Given `"file:///tmp/logs/cache/artifacts/values/response.plan.json"`, returns
/// `"See: /tmp/logs/cache/artifacts/values/response.plan.json"`.
#[must_use]
pub fn format_artifact_reference(path: &str) -> String {
    format!("See: {path}")
}

/// Sync artifact files to a remote sandbox.
///
/// For each `file://` pointer in `updates`, checks whether the file is accessible
/// in `env`. If not, reads the local file and uploads it via `env.write_file`,
/// placing it at `{working_directory}/.fabro/artifacts/{filename}`. The pointer
/// is rewritten to reference the remote path.
///
/// # Errors
///
/// Returns an error if reading a local artifact or writing to the remote env fails.
pub async fn sync_artifacts_to_env(
    updates: &mut HashMap<String, Value>,
    env: &dyn Sandbox,
) -> Result<()> {
    for value in updates.values_mut() {
        let local_path = match artifact_path(value) {
            Some(p) => p.to_string(),
            None => continue,
        };

        match env.file_exists(&local_path).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                return Err(FabroError::engine(format!(
                    "failed to check artifact existence: {e}"
                )));
            }
        }

        let content = std::fs::read_to_string(&local_path).map_err(|e| {
            FabroError::engine(format!("failed to read local artifact {local_path}: {e}"))
        })?;

        let filename = std::path::Path::new(&local_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("artifact.json");

        let remote_path = format!("{}/.fabro/artifacts/{filename}", env.working_directory());

        env.write_file(&remote_path, &content).await.map_err(|e| {
            FabroError::engine(format!("failed to write artifact to remote env: {e}"))
        })?;

        *value = Value::String(format!("{ARTIFACT_POINTER_PREFIX}{remote_path}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve_small_artifact() {
        let store = ArtifactStore::new(None);
        let data = serde_json::json!({"result": "ok"});
        let info = store.store("art1", "test artifact", data.clone()).unwrap();

        assert_eq!(info.id, "art1");
        assert_eq!(info.name, "test artifact");
        assert!(!info.is_file_backed);
        assert!(info.size_bytes > 0);
        assert!(info.file_path.is_none());

        let retrieved = store.retrieve("art1").unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn retrieve_nonexistent() {
        let store = ArtifactStore::new(None);
        assert!(store.retrieve("missing").is_err());
    }

    #[test]
    fn has_artifact() {
        let store = ArtifactStore::new(None);
        assert!(!store.has("x"));
        store.store("x", "x", serde_json::json!(1)).unwrap();
        assert!(store.has("x"));
    }

    #[test]
    fn list_artifacts() {
        let store = ArtifactStore::new(None);
        store.store("a", "alpha", serde_json::json!(1)).unwrap();
        store.store("b", "beta", serde_json::json!(2)).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn remove_artifact() {
        let store = ArtifactStore::new(None);
        store.store("r", "remove me", serde_json::json!(1)).unwrap();
        assert!(store.has("r"));
        store.remove("r");
        assert!(!store.has("r"));
    }

    #[test]
    fn clear_artifacts() {
        let store = ArtifactStore::new(None);
        store.store("a", "a", serde_json::json!(1)).unwrap();
        store.store("b", "b", serde_json::json!(2)).unwrap();
        assert_eq!(store.list().len(), 2);
        store.clear();
        assert!(store.list().is_empty());
    }

    #[test]
    fn file_backed_storage() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(Some(dir.path().to_path_buf()));

        // Create data larger than the 100KB threshold
        let large_string = "x".repeat(FILE_BACKING_THRESHOLD + 1);
        let data = serde_json::json!(large_string);

        let info = store.store("big", "large artifact", data.clone()).unwrap();
        assert!(info.is_file_backed);
        assert!(info.size_bytes > FILE_BACKING_THRESHOLD);
        assert_eq!(info.file_path, Some(dir.path().join("big.json")));

        let retrieved = store.retrieve("big").unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn file_backed_remove_deletes_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(Some(dir.path().to_path_buf()));

        let large_string = "x".repeat(FILE_BACKING_THRESHOLD + 1);
        let data = serde_json::json!(large_string);
        store.store("big", "large", data).unwrap();

        let file_path = dir.path().join("big.json");
        assert!(file_path.exists());

        store.remove("big");
        assert!(!file_path.exists());
    }

    #[test]
    fn small_artifact_stays_in_memory_even_with_values_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(Some(dir.path().to_path_buf()));

        let data = serde_json::json!({"small": true});
        let info = store.store("small", "tiny", data).unwrap();
        assert!(!info.is_file_backed);
    }

    #[test]
    fn no_file_backing_without_base_dir() {
        let store = ArtifactStore::new(None);

        let large_string = "x".repeat(FILE_BACKING_THRESHOLD + 1);
        let data = serde_json::json!(large_string);
        let info = store.store("big", "large", data).unwrap();
        assert!(!info.is_file_backed);
    }

    #[test]
    fn offload_replaces_large_values_with_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(Some(dir.path().to_path_buf()));

        let large_string = "x".repeat(FILE_BACKING_THRESHOLD + 1);
        let mut updates = HashMap::new();
        updates.insert("response.plan".to_string(), serde_json::json!(large_string));

        offload_large_values(&mut updates, &store).unwrap();

        // Value should now be a pointer string
        let pointer = updates.get("response.plan").unwrap();
        let path = artifact_path(pointer).expect("should be an artifact pointer");
        assert_eq!(
            path,
            dir.path().join("response.plan.json").to_str().unwrap()
        );

        // The artifact store should contain the original value
        let retrieved = store.retrieve("response.plan").unwrap();
        assert_eq!(retrieved, serde_json::json!(large_string));

        // File should exist on disk
        assert!(dir.path().join("response.plan.json").exists());
    }

    #[test]
    fn offload_leaves_small_values_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(Some(dir.path().to_path_buf()));

        let small_value = serde_json::json!("hello world");
        let mut updates = HashMap::new();
        updates.insert("small_key".to_string(), small_value.clone());

        offload_large_values(&mut updates, &store).unwrap();

        assert_eq!(updates.get("small_key").unwrap(), &small_value);
        assert!(!store.has("small_key"));
    }

    #[test]
    fn artifact_path_extracts_path_from_pointer() {
        let value = serde_json::json!("file:///tmp/logs/cache/artifacts/values/response.plan.json");
        assert_eq!(
            artifact_path(&value),
            Some("/tmp/logs/cache/artifacts/values/response.plan.json")
        );
    }

    #[test]
    fn values_dir_returns_configured_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(Some(dir.path().to_path_buf()));

        assert_eq!(store.values_dir(), Some(dir.path().to_path_buf()));
    }

    #[test]
    fn artifact_path_returns_none_for_plain_string() {
        let value = serde_json::json!("just a normal string");
        assert_eq!(artifact_path(&value), None);
    }

    #[test]
    fn artifact_path_returns_none_for_non_string() {
        let value = serde_json::json!(42);
        assert_eq!(artifact_path(&value), None);
    }

    // --- sync_artifacts_to_env tests ---

    use std::sync::Mutex;

    struct TestSyncEnv {
        accessible: bool,
        written: Mutex<Vec<(String, String)>>,
        working_dir: String,
    }

    impl TestSyncEnv {
        fn new(accessible: bool, working_dir: &str) -> Self {
            Self {
                accessible,
                written: Mutex::new(Vec::new()),
                working_dir: working_dir.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl Sandbox for TestSyncEnv {
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<usize>,
            _limit: Option<usize>,
        ) -> std::result::Result<String, String> {
            Err("not implemented".to_string())
        }

        async fn write_file(&self, path: &str, content: &str) -> std::result::Result<(), String> {
            self.written
                .lock()
                .unwrap()
                .push((path.to_string(), content.to_string()));
            Ok(())
        }

        async fn delete_file(&self, _path: &str) -> std::result::Result<(), String> {
            Err("not implemented".to_string())
        }

        async fn file_exists(&self, _path: &str) -> std::result::Result<bool, String> {
            Ok(self.accessible)
        }

        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> std::result::Result<Vec<fabro_agent::DirEntry>, String> {
            Err("not implemented".to_string())
        }

        async fn exec_command(
            &self,
            _command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            _cancel_token: Option<tokio_util::sync::CancellationToken>,
        ) -> std::result::Result<fabro_agent::ExecResult, String> {
            Err("not implemented".to_string())
        }

        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &fabro_agent::GrepOptions,
        ) -> std::result::Result<Vec<String>, String> {
            Err("not implemented".to_string())
        }

        async fn glob(
            &self,
            _pattern: &str,
            _path: Option<&str>,
        ) -> std::result::Result<Vec<String>, String> {
            Err("not implemented".to_string())
        }

        async fn download_file_to_local(
            &self,
            _remote_path: &str,
            _local_path: &std::path::Path,
        ) -> std::result::Result<(), String> {
            Err("not implemented".to_string())
        }

        async fn upload_file_from_local(
            &self,
            _local_path: &std::path::Path,
            _remote_path: &str,
        ) -> std::result::Result<(), String> {
            Err("not implemented".to_string())
        }

        async fn initialize(&self) -> std::result::Result<(), String> {
            Ok(())
        }

        async fn cleanup(&self) -> std::result::Result<(), String> {
            Ok(())
        }

        fn working_directory(&self) -> &str {
            &self.working_dir
        }

        fn platform(&self) -> &str {
            "linux"
        }

        fn os_version(&self) -> String {
            "Linux 5.15".to_string()
        }
    }

    #[tokio::test]
    async fn sync_uploads_artifact_when_not_accessible() {
        let dir = tempfile::tempdir().unwrap();
        let artifact_file = dir.path().join("response.plan.json");
        std::fs::write(&artifact_file, r#""hello from artifact""#).unwrap();

        let pointer = format!("file://{}", artifact_file.display());
        let mut updates = HashMap::new();
        updates.insert("response.plan".to_string(), Value::String(pointer));

        let env = TestSyncEnv::new(false, "/workspace");
        sync_artifacts_to_env(&mut updates, &env).await.unwrap();

        let written = env.written.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(
            written[0].0,
            "/workspace/.fabro/artifacts/response.plan.json"
        );
        assert_eq!(written[0].1, r#""hello from artifact""#);

        let new_pointer = updates["response.plan"].as_str().unwrap();
        assert_eq!(
            new_pointer,
            "file:///workspace/.fabro/artifacts/response.plan.json"
        );
    }

    #[tokio::test]
    async fn sync_skips_when_artifact_already_accessible() {
        let dir = tempfile::tempdir().unwrap();
        let artifact_file = dir.path().join("data.json");
        std::fs::write(&artifact_file, "{}").unwrap();

        let pointer = format!("file://{}", artifact_file.display());
        let mut updates = HashMap::new();
        updates.insert("key".to_string(), Value::String(pointer.clone()));

        let env = TestSyncEnv::new(true, "/workspace");
        sync_artifacts_to_env(&mut updates, &env).await.unwrap();

        let written = env.written.lock().unwrap();
        assert!(written.is_empty());
        assert_eq!(updates["key"].as_str().unwrap(), &pointer);
    }

    #[tokio::test]
    async fn sync_ignores_non_artifact_values() {
        let mut updates = HashMap::new();
        updates.insert("name".to_string(), serde_json::json!("Alice"));
        updates.insert("count".to_string(), serde_json::json!(42));
        updates.insert("nested".to_string(), serde_json::json!({"a": 1}));

        let env = TestSyncEnv::new(false, "/workspace");
        sync_artifacts_to_env(&mut updates, &env).await.unwrap();

        let written = env.written.lock().unwrap();
        assert!(written.is_empty());
        assert_eq!(updates["name"], serde_json::json!("Alice"));
        assert_eq!(updates["count"], serde_json::json!(42));
        assert_eq!(updates["nested"], serde_json::json!({"a": 1}));
    }
}
