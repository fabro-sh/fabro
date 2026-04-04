use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use fabro_agent::Sandbox;
use fabro_store::SlateRunStore;

use crate::error::{FabroError, Result};

/// Threshold above which values are persisted as blobs and materialized to disk (100KB).
const BLOB_OFFLOAD_THRESHOLD: usize = 100 * 1024;

/// Prefix used to identify artifact pointer strings in context values.
const ARTIFACT_POINTER_PREFIX: &str = "file://";

/// Offload context values exceeding the blob threshold into SlateDB and materialize cache files.
///
/// For each entry in `updates` whose serialized JSON exceeds `BLOB_OFFLOAD_THRESHOLD`,
/// the value is persisted as a blob in `run_store`, materialized in `cache_dir`, and
/// replaced with a `"file://{path}"` pointer.
/// Small values are left untouched.
///
/// # Errors
///
/// Returns an error if blob persistence or cache materialization fails.
pub async fn offload_large_values(
    updates: &mut HashMap<String, Value>,
    run_store: &SlateRunStore,
    cache_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;

    for value in updates.values_mut() {
        let bytes = serde_json::to_vec(&*value)
            .map_err(|e| FabroError::engine(format!("artifact serialize failed: {e}")))?;

        if bytes.len() > BLOB_OFFLOAD_THRESHOLD {
            let blob_id = run_store
                .write_blob(&bytes)
                .await
                .map_err(|e| FabroError::engine(format!("artifact blob write failed: {e}")))?;
            let cache_path = cache_dir.join(format!("{blob_id}.json"));
            if !cache_path.exists() {
                std::fs::write(&cache_path, &bytes)?;
            }
            *value = Value::String(format!("{ARTIFACT_POINTER_PREFIX}{}", cache_path.display()));
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
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use fabro_store::SlateStore;
    use object_store::memory::InMemory;
    use ulid::Ulid;

    fn test_run_id(label: &str) -> fabro_types::RunId {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        label.hash(&mut hasher);
        fabro_types::RunId::from(Ulid(u128::from(hasher.finish())))
    }

    async fn make_run_store(label: &str) -> fabro_store::SlateRunStore {
        let object_store = Arc::new(InMemory::new());
        let store = SlateStore::new(object_store, "runs/", Duration::from_millis(1));
        store.create_run(&test_run_id(label)).await.unwrap()
    }

    #[tokio::test]
    async fn offload_replaces_large_values_with_blob_backed_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let run_store = make_run_store("artifact-offload").await;

        let large_string = "x".repeat(BLOB_OFFLOAD_THRESHOLD + 1);
        let serialized = serde_json::to_vec(&serde_json::json!(large_string.clone())).unwrap();
        let expected_blob_id =
            fabro_types::RunBlobId::new(&test_run_id("artifact-offload"), &serialized);

        let mut updates = HashMap::new();
        updates.insert("response.plan".to_string(), serde_json::json!(large_string));

        offload_large_values(&mut updates, &run_store, dir.path())
            .await
            .unwrap();

        let pointer = updates.get("response.plan").unwrap();
        let path = artifact_path(pointer).expect("should be an artifact pointer");
        assert_eq!(
            path,
            dir.path()
                .join(format!("{expected_blob_id}.json"))
                .to_str()
                .unwrap()
        );

        let blob = run_store
            .read_blob(&expected_blob_id)
            .await
            .unwrap()
            .expect("blob should exist");
        let blob_value: serde_json::Value = serde_json::from_slice(&blob).unwrap();
        assert_eq!(blob_value, serde_json::json!(large_string));
        assert!(
            dir.path().join(format!("{expected_blob_id}.json")).exists(),
            "materialized cache file should exist"
        );
    }

    #[tokio::test]
    async fn offload_leaves_small_values_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let run_store = make_run_store("artifact-small").await;
        let small_value = serde_json::json!("hello world");
        let mut updates = HashMap::new();
        updates.insert("small_key".to_string(), small_value.clone());

        offload_large_values(&mut updates, &run_store, dir.path())
            .await
            .unwrap();

        assert_eq!(updates.get("small_key").unwrap(), &small_value);
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
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
