use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fabro_agent::Sandbox;
use fabro_config::RunScratch;
use fabro_types::{
    RunBlobId, format_blob_ref, parse_blob_ref, parse_legacy_blob_file_ref,
    parse_managed_blob_file_ref,
};
use futures::future::BoxFuture;
use serde_json::Value;
use tokio::fs;

use crate::context::{self, Context};
use crate::error::{Error, Result};
use crate::outcome::Outcome;
use crate::records::Checkpoint;
use crate::runtime_store::RunStoreHandle;

/// Threshold above which values are persisted as blobs (100KB).
const BLOB_OFFLOAD_THRESHOLD: usize = 100 * 1024;

/// Prefix used to identify artifact pointer strings in context values.
const ARTIFACT_POINTER_PREFIX: &str = "file://";

/// Offload context values exceeding the blob threshold into the blob store.
///
/// For each entry in `updates` whose serialized JSON exceeds
/// `BLOB_OFFLOAD_THRESHOLD`, the value is persisted as a blob in `run_store`
/// and replaced with a `"blob://sha256/{blob_id}"` reference.
/// Small values are left untouched.
///
/// # Errors
///
/// Returns an error if blob persistence fails.
pub async fn offload_large_values(
    updates: &mut HashMap<String, Value>,
    run_store: &RunStoreHandle,
) -> Result<()> {
    for value in updates.values_mut() {
        let bytes = serde_json::to_vec(&*value)
            .map_err(|e| Error::engine(format!("artifact serialize failed: {e}")))?;

        if bytes.len() > BLOB_OFFLOAD_THRESHOLD {
            let blob_id = run_store
                .write_blob(&bytes)
                .await
                .map_err(|e| Error::engine(format!("artifact blob write failed: {e}")))?;
            *value = Value::String(format_blob_ref(&blob_id));
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

/// Resolve an artifact pointer to the base name displayed in preamble
/// rendering.
///
/// Given `"file:///tmp/logs/runtime/blobs/response.plan.json"`, returns
/// `"See: /tmp/logs/runtime/blobs/response.plan.json"`.
#[must_use]
pub fn format_artifact_reference(path: &str) -> String {
    format!("See: {path}")
}

pub fn durable_context_snapshot(context: &Context) -> HashMap<String, Value> {
    let mut snapshot = context.snapshot();
    snapshot.remove(context::keys::CURRENT_PREAMBLE);
    normalize_durable_updates(&mut snapshot);
    snapshot
}

pub fn normalize_durable_updates(updates: &mut HashMap<String, Value>) {
    for value in updates.values_mut() {
        normalize_durable_value(value);
    }
}

pub fn normalize_durable_outcomes(node_outcomes: &mut HashMap<String, Outcome>) {
    for outcome in node_outcomes.values_mut() {
        normalize_durable_updates(&mut outcome.context_updates);
    }
}

pub fn normalize_checkpoint_for_resume(checkpoint: &mut Checkpoint) {
    checkpoint
        .context_values
        .remove(context::keys::CURRENT_PREAMBLE);
    normalize_durable_updates(&mut checkpoint.context_values);
    normalize_durable_outcomes(&mut checkpoint.node_outcomes);
}

pub async fn resolve_context_for_execution(
    context: &Context,
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
) -> Result<Context> {
    let values = resolved_context_snapshot(context, run_store, env, run_dir).await?;
    let resolved = Context::new();
    for (key, value) in values {
        resolved.set(key, value);
    }
    Ok(resolved)
}

pub async fn resolve_outcomes_for_execution(
    node_outcomes: &HashMap<String, Outcome>,
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
) -> Result<HashMap<String, Outcome>> {
    let mut resolved = node_outcomes.clone();
    for outcome in resolved.values_mut() {
        resolve_execution_values(&mut outcome.context_updates, run_store, env, run_dir).await?;
    }
    Ok(resolved)
}

pub async fn resolved_context_snapshot(
    context: &Context,
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
) -> Result<HashMap<String, Value>> {
    let mut values = context.snapshot();
    resolve_execution_values(&mut values, run_store, env, run_dir).await?;
    Ok(values)
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
/// Returns an error if reading a local artifact or writing to the remote env
/// fails.
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
                return Err(Error::engine(format!(
                    "failed to check artifact existence: {e}"
                )));
            }
        }

        let content = fs::read_to_string(&local_path).await.map_err(|e| {
            Error::engine(format!("failed to read local artifact {local_path}: {e}"))
        })?;

        let filename = std::path::Path::new(&local_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("artifact.json");

        let remote_path = format!("{}/.fabro/artifacts/{filename}", env.working_directory());

        env.write_file(&remote_path, &content)
            .await
            .map_err(|e| Error::engine(format!("failed to write artifact to remote env: {e}")))?;

        *value = Value::String(format!("{ARTIFACT_POINTER_PREFIX}{remote_path}"));
    }
    Ok(())
}

fn normalize_durable_value(value: &mut Value) {
    match value {
        Value::String(current) => {
            if let Some(blob_id) =
                parse_legacy_blob_file_ref(current).or_else(|| parse_managed_blob_file_ref(current))
            {
                *current = format_blob_ref(&blob_id);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_durable_value(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                normalize_durable_value(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn resolve_execution_values<'a>(
    values: &'a mut HashMap<String, Value>,
    run_store: &'a RunStoreHandle,
    env: &'a dyn Sandbox,
    run_dir: &'a Path,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        for value in values.values_mut() {
            resolve_execution_value(value, run_store, env, run_dir).await?;
        }
        Ok(())
    })
}

fn resolve_execution_value<'a>(
    value: &'a mut Value,
    run_store: &'a RunStoreHandle,
    env: &'a dyn Sandbox,
    run_dir: &'a Path,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        match value {
            Value::String(current) => {
                if let Some(blob_id) =
                    parse_blob_ref(current).or_else(|| parse_legacy_blob_file_ref(current))
                {
                    *current = materialize_blob_ref(&blob_id, run_store, env, run_dir).await?;
                } else if current.starts_with(ARTIFACT_POINTER_PREFIX)
                    && parse_managed_blob_file_ref(current).is_none()
                {
                    *current = resolve_explicit_file_ref(current, env).await?;
                }
            }
            Value::Array(items) => {
                for item in items {
                    resolve_execution_value(item, run_store, env, run_dir).await?;
                }
            }
            Value::Object(map) => {
                for item in map.values_mut() {
                    resolve_execution_value(item, run_store, env, run_dir).await?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    })
}

async fn materialize_blob_ref(
    blob_id: &RunBlobId,
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
) -> Result<String> {
    let bytes = run_store
        .read_blob(blob_id)
        .await
        .map_err(|e| Error::engine(format!("artifact blob read failed: {e}")))?
        .ok_or_else(|| Error::engine(format!("artifact blob missing: {blob_id}")))?;

    if is_local_execution(env, run_dir).await? {
        let path = local_materialized_blob_path(run_dir, blob_id);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).await.map_err(|err| {
                    Error::Io(format!(
                        "creating artifact blob directory {}: {err}",
                        parent.display()
                    ))
                })?;
            }
            fs::write(&path, &bytes).await.map_err(|err| {
                Error::Io(format!("writing artifact blob {}: {err}", path.display()))
            })?;
        }
        return Ok(format!("{ARTIFACT_POINTER_PREFIX}{}", path.display()));
    }

    let remote_path = format!("{}/.fabro/blobs/{blob_id}.json", env.working_directory());
    if !env
        .file_exists(&remote_path)
        .await
        .map_err(|e| Error::engine(format!("failed to check blob existence: {e}")))?
    {
        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| Error::engine(format!("artifact blob was not valid UTF-8 JSON: {e}")))?;
        env.write_file(&remote_path, &content)
            .await
            .map_err(|e| Error::engine(format!("failed to write artifact blob to sandbox: {e}")))?;
    }

    Ok(format!("{ARTIFACT_POINTER_PREFIX}{remote_path}"))
}

async fn resolve_explicit_file_ref(value: &str, env: &dyn Sandbox) -> Result<String> {
    let local_path = value
        .strip_prefix(ARTIFACT_POINTER_PREFIX)
        .ok_or_else(|| Error::engine(format!("invalid artifact pointer: {value}")))?;

    if env
        .file_exists(local_path)
        .await
        .map_err(|e| Error::engine(format!("failed to check artifact existence: {e}")))?
    {
        return Ok(value.to_string());
    }

    let content = fs::read_to_string(local_path)
        .await
        .map_err(|e| Error::engine(format!("failed to read local artifact {local_path}: {e}")))?;
    let filename = Path::new(local_path)
        .file_name()
        .and_then(|file| file.to_str())
        .unwrap_or("artifact.json");
    let remote_path = format!("{}/.fabro/artifacts/{filename}", env.working_directory());

    if !env
        .file_exists(&remote_path)
        .await
        .map_err(|e| Error::engine(format!("failed to check artifact existence: {e}")))?
    {
        env.write_file(&remote_path, &content)
            .await
            .map_err(|e| Error::engine(format!("failed to write artifact to remote env: {e}")))?;
    }

    Ok(format!("{ARTIFACT_POINTER_PREFIX}{remote_path}"))
}

async fn is_local_execution(env: &dyn Sandbox, run_dir: &Path) -> Result<bool> {
    env.file_exists(&run_dir.to_string_lossy())
        .await
        .map_err(|e| Error::engine(format!("failed to inspect sandbox locality: {e}")))
}

fn local_materialized_blob_path(run_dir: &Path, blob_id: &RunBlobId) -> PathBuf {
    RunScratch::new(run_dir)
        .runtime_dir()
        .join("blobs")
        .join(format!("{blob_id}.json"))
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests write artifact fixtures to disk"
)]
mod tests {
    use std::hash::{Hash, Hasher};
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_store::Database;
    use object_store::memory::InMemory;
    use ulid::Ulid;

    use super::*;

    fn test_run_id(label: &str) -> fabro_types::RunId {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        label.hash(&mut hasher);
        fabro_types::RunId::from(Ulid(u128::from(hasher.finish())))
    }

    async fn make_run_store(label: &str) -> fabro_store::RunDatabase {
        let object_store = Arc::new(InMemory::new());
        let store = Database::new(object_store, "runs/", Duration::from_millis(1), None);
        store.create_run(&test_run_id(label)).await.unwrap()
    }

    #[tokio::test]
    async fn offload_replaces_large_values_with_blob_backed_pointer() {
        let run_store = make_run_store("artifact-offload").await;

        let large_string = "x".repeat(BLOB_OFFLOAD_THRESHOLD + 1);
        let serialized = serde_json::to_vec(&serde_json::json!(large_string.clone())).unwrap();
        let expected_blob_id = fabro_types::RunBlobId::new(&serialized);

        let mut updates = HashMap::new();
        updates.insert("response.plan".to_string(), serde_json::json!(large_string));

        offload_large_values(&mut updates, &run_store.clone().into())
            .await
            .unwrap();

        let pointer = updates.get("response.plan").unwrap();
        assert_eq!(
            pointer,
            &serde_json::json!(fabro_types::format_blob_ref(&expected_blob_id))
        );

        let blob = run_store
            .read_blob(&expected_blob_id)
            .await
            .unwrap()
            .expect("blob should exist");
        let blob_value: serde_json::Value = serde_json::from_slice(&blob).unwrap();
        assert_eq!(blob_value, serde_json::json!(large_string));
    }

    #[tokio::test]
    async fn offload_leaves_small_values_untouched() {
        let run_store = make_run_store("artifact-small").await;
        let small_value = serde_json::json!("hello world");
        let mut updates = HashMap::new();
        updates.insert("small_key".to_string(), small_value.clone());

        offload_large_values(&mut updates, &run_store.clone().into())
            .await
            .unwrap();

        assert_eq!(updates.get("small_key").unwrap(), &small_value);
    }

    #[test]
    fn artifact_path_extracts_path_from_pointer() {
        let value = serde_json::json!("file:///tmp/logs/runtime/blobs/response.plan.json");
        assert_eq!(
            artifact_path(&value),
            Some("/tmp/logs/runtime/blobs/response.plan.json")
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

    #[test]
    fn normalize_durable_updates_rewrites_managed_blob_file_refs_recursively() {
        let blob_id = fabro_types::RunBlobId::new(b"hello");
        let mut updates = HashMap::from([(
            "nested".to_string(),
            serde_json::json!({
                "items": [
                    format!("file:///tmp/run/runtime/blobs/{blob_id}.json"),
                    format!("file:///sandbox/.fabro/blobs/{blob_id}.json"),
                    "file:///tmp/report.json",
                ]
            }),
        )]);

        normalize_durable_updates(&mut updates);

        assert_eq!(
            updates["nested"],
            serde_json::json!({
                "items": [
                    fabro_types::format_blob_ref(&blob_id),
                    fabro_types::format_blob_ref(&blob_id),
                    "file:///tmp/report.json",
                ]
            })
        );
    }

    #[test]
    fn normalize_checkpoint_for_resume_converts_legacy_blob_file_refs_and_drops_preamble() {
        let blob_id = fabro_types::RunBlobId::new(b"legacy");
        let mut checkpoint = crate::records::Checkpoint {
            timestamp:                  chrono::Utc::now(),
            current_node:               "work".to_string(),
            completed_nodes:            vec!["work".to_string()],
            node_retries:               HashMap::new(),
            context_values:             HashMap::from([
                (
                    crate::context::keys::CURRENT_PREAMBLE.to_string(),
                    serde_json::json!("runtime only"),
                ),
                (
                    "response.work".to_string(),
                    serde_json::json!(format!("file:///sandbox/.fabro/artifacts/{blob_id}.json")),
                ),
            ]),
            node_outcomes:              HashMap::from([(
                "work".to_string(),
                crate::outcome::Outcome {
                    context_updates: HashMap::from([(
                        "response.work".to_string(),
                        serde_json::json!(format!(
                            "file:///sandbox/.fabro/artifacts/{blob_id}.json"
                        )),
                    )]),
                    ..crate::outcome::Outcome::success()
                },
            )]),
            next_node_id:               Some("exit".to_string()),
            git_commit_sha:             None,
            loop_failure_signatures:    HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits:                HashMap::new(),
        };

        normalize_checkpoint_for_resume(&mut checkpoint);

        assert!(
            !checkpoint
                .context_values
                .contains_key(crate::context::keys::CURRENT_PREAMBLE)
        );
        assert_eq!(
            checkpoint.context_values.get("response.work"),
            Some(&serde_json::json!(fabro_types::format_blob_ref(&blob_id)))
        );
        assert_eq!(
            checkpoint
                .node_outcomes
                .get("work")
                .and_then(|outcome| outcome.context_updates.get("response.work")),
            Some(&serde_json::json!(fabro_types::format_blob_ref(&blob_id)))
        );
    }

    // --- sync_artifacts_to_env tests ---

    use std::sync::Mutex;

    struct TestSyncEnv {
        accessible:  bool,
        written:     Mutex<Vec<(String, String)>>,
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
