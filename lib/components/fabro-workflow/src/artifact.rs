use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use fabro_agent::Sandbox;
use fabro_config::RunScratch;
use fabro_types::{
    BlobHash, ParallelBranchResult, format_blob_ref, parse_blob_ref, parse_managed_blob_file_ref,
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

/// Largest serialized JSON one context or outcome value may contribute to a
/// prompt preamble before it is demoted to a preview plus a file reference.
const PROMPT_INLINE_VALUE_MAX: usize = 8 * 1024;

/// Largest serialized JSON one `for_each` item may contribute to a branch
/// prompt before it is demoted. The item is the branch's work assignment, so
/// its budget is deliberately more generous than [`PROMPT_INLINE_VALUE_MAX`].
const PROMPT_INLINE_ITEM_MAX: usize = 64 * 1024;

/// Rendered head carried inline by a demotion marker so the reader can tell
/// what the value is without opening the file.
const LARGE_VALUE_PREVIEW_CHARS: usize = 300;

const LARGE_VALUE_MARKER_KEY: &str = "fabroLargeValue";
const LARGE_VALUE_HINT: &str = "too large to inline; read this file for the full value";

/// Prefix used to identify artifact pointer strings in context values.
const ARTIFACT_POINTER_PREFIX: &str = "file://";

/// Prompt-facing details held by an internal large-value marker.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromptLargeValue<'a> {
    pub bytes:   u64,
    pub path:    &'a str,
    pub preview: &'a str,
}

impl PromptLargeValue<'_> {
    /// Concise metadata shown next to the context key or stage-output label.
    #[must_use]
    pub(crate) fn location_summary(self) -> String {
        format!(
            "{}; full value: `{}`",
            format_prompt_bytes(self.bytes),
            self.path
        )
    }
}

/// Offload context values exceeding the blob threshold into the blob store.
///
/// For each entry in `updates` whose serialized JSON exceeds
/// `BLOB_OFFLOAD_THRESHOLD`, the value is persisted as a blob in `run_store`
/// and replaced with a `"blob://sha256/{blob_hash}"` reference.
/// Small values are left untouched.
///
/// `parallel.results` is offloaded at each branch context-update boundary
/// instead of as one value so it stays a structured array that fan-in prompts,
/// projections, and the UI can read without hydrating the whole payload.
///
/// # Errors
///
/// Returns an error if blob persistence fails.
pub async fn offload_large_values(
    updates: &mut HashMap<String, Value>,
    run_store: &RunStoreHandle,
) -> Result<()> {
    for (key, value) in updates {
        if key == context::keys::PARALLEL_RESULTS {
            offload_parallel_result_updates(value, run_store).await?;
        } else {
            offload_value(value, run_store).await?;
        }
    }
    Ok(())
}

/// Offload large context-update values from typed parallel branch results
/// before they are emitted through `parallel.completed` and stored in
/// projections.
///
/// # Errors
///
/// Returns an error if blob persistence fails.
pub async fn offload_parallel_branch_updates(
    results: &mut [ParallelBranchResult],
    run_store: &RunStoreHandle,
) -> Result<()> {
    for result in results.iter_mut() {
        for value in result.context_updates.values_mut() {
            offload_value(value, run_store).await?;
        }
    }
    Ok(())
}

async fn offload_parallel_result_updates(
    value: &mut Value,
    run_store: &RunStoreHandle,
) -> Result<()> {
    let Some(results) = value.as_array_mut() else {
        return Ok(());
    };
    for result in results {
        let Some(context_updates) = result
            .get_mut("context_updates")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        for value in context_updates.values_mut() {
            offload_value(value, run_store).await?;
        }
    }
    Ok(())
}

async fn offload_value(value: &mut Value, run_store: &RunStoreHandle) -> Result<()> {
    let Some(bytes) = serialized_if_over(value, BLOB_OFFLOAD_THRESHOLD)? else {
        return Ok(());
    };
    let blob_hash = run_store
        .write_blob(&bytes)
        .await
        .map_err(|e| Error::engine_with_anyhow("artifact blob write failed", e))?;
    *value = Value::String(format_blob_ref(&blob_hash));
    Ok(())
}

/// Serialize `value` only when it can exceed `threshold` bytes, returning the
/// serialized form when it does.
fn serialized_if_over(value: &Value, threshold: usize) -> Result<Option<Vec<u8>>> {
    match value {
        // Scalars can never reach an offload threshold.
        Value::Null | Value::Bool(_) | Value::Number(_) => return Ok(None),
        // JSON escaping expands a string to at most 6 bytes per char plus
        // quotes, so short strings can never cross the threshold — skip
        // serializing them.
        Value::String(text) if text.len().saturating_mul(6) + 2 <= threshold => return Ok(None),
        _ => {}
    }
    let bytes = serde_json::to_vec(value)
        .map_err(|e| Error::engine_with_source("artifact serialize failed", e))?;
    Ok((bytes.len() > threshold).then_some(bytes))
}

/// The exact set of resolved context and outcome values the preamble builders
/// will render for the fidelities in play, keyed the way the demotion pass
/// looks them up.
///
/// Built by `preamble::rendered_value_selection` for one fidelity and merged
/// across a parallel node's branch fidelities, so demotion materializes only
/// values some generated preamble actually references.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PromptValueSelection {
    context_keys: HashSet<String>,
    outcome_keys: HashMap<String, HashSet<String>>,
}

impl PromptValueSelection {
    /// Mark a resolved context snapshot key as rendered.
    pub fn select_context_value(&mut self, key: &str) {
        self.context_keys.insert(key.to_string());
    }

    /// Mark one key of a completed node's outcome context updates as rendered.
    pub fn select_outcome_value(&mut self, node_id: &str, key: &str) {
        self.outcome_keys
            .entry(node_id.to_string())
            .or_default()
            .insert(key.to_string());
    }

    /// Union another fidelity's selection into this one.
    pub fn merge(&mut self, other: Self) {
        self.context_keys.extend(other.context_keys);
        for (node_id, keys) in other.outcome_keys {
            self.outcome_keys.entry(node_id).or_default().extend(keys);
        }
    }

    /// True when no preamble in play renders any value, so demotion has
    /// nothing to bound.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.context_keys.is_empty() && self.outcome_keys.is_empty()
    }

    pub(crate) fn renders_context_value(&self, key: &str) -> bool {
        self.context_keys.contains(key)
    }

    pub(crate) fn renders_outcome_value(&self, node_id: &str, key: &str) -> bool {
        self.outcome_keys
            .get(node_id)
            .is_some_and(|keys| keys.contains(key))
    }
}

/// Bound every value the prompt preamble will inline.
///
/// The resolved context snapshot and outcomes passed here exist only to
/// render prompt text, so any selected value whose serialized JSON exceeds
/// [`PROMPT_INLINE_VALUE_MAX`] is replaced with a small marker object holding
/// a preview and the sandbox path of the full value. The agent reads the file
/// when it needs the data; the preamble stays within its budget no matter how
/// much state the run has accumulated.
///
/// Only values named by `selection` are demoted: those are exactly the values
/// the selected fidelity's preamble path renders, so no blob or sandbox file
/// is created for a value the generated prompt omits.
///
/// Demotion is an optimization of prompt size, not a correctness gate: a
/// value that fails to demote is left inline and logged rather than failing
/// the node.
pub async fn demote_large_values_for_prompt(
    values: &mut HashMap<String, Value>,
    node_outcomes: &mut HashMap<String, Outcome>,
    selection: &PromptValueSelection,
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
) {
    let mut locality = SandboxLocality::default();
    for (key, value) in &mut *values {
        if !selection.renders_context_value(key) {
            continue;
        }
        if let Err(err) = demote_value_for_prompt(
            value,
            PROMPT_INLINE_VALUE_MAX,
            run_store,
            env,
            run_dir,
            &mut locality,
        )
        .await
        {
            tracing::warn!(key, %err, "prompt value demotion failed; kept inline");
        }
    }
    for (node_id, outcome) in &mut *node_outcomes {
        for (key, value) in &mut outcome.context_updates {
            if !selection.renders_outcome_value(node_id, key) {
                continue;
            }
            if let Err(err) = demote_value_for_prompt(
                value,
                PROMPT_INLINE_VALUE_MAX,
                run_store,
                env,
                run_dir,
                &mut locality,
            )
            .await
            {
                tracing::warn!(
                    node_id,
                    key,
                    %err,
                    "prompt value demotion failed; kept inline"
                );
            }
        }
    }
}

/// Bound every `for_each` item rendered into a branch prompt.
///
/// Items above [`PROMPT_INLINE_ITEM_MAX`] are demoted the same way as context
/// values; the branch reads the file for its full assignment. An item that
/// fails to demote is left inline and logged.
pub async fn demote_large_items_for_prompt(
    items: &mut [Value],
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
) {
    let mut locality = SandboxLocality::default();
    for (index, item) in items.iter_mut().enumerate() {
        if let Err(err) = demote_value_for_prompt(
            item,
            PROMPT_INLINE_ITEM_MAX,
            run_store,
            env,
            run_dir,
            &mut locality,
        )
        .await
        {
            tracing::warn!(index, %err, "for_each item demotion failed; kept inline");
        }
    }
}

/// Replace `value` with a preview-plus-path marker when its serialized JSON
/// exceeds `max_inline_bytes`. Returns whether the value was demoted.
///
/// The full value is persisted as a content-addressed blob and materialized
/// as a real file in the sandbox, so the marker's `path` is readable by the
/// agent that receives the prompt.
async fn demote_value_for_prompt(
    value: &mut Value,
    max_inline_bytes: usize,
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
    locality: &mut SandboxLocality,
) -> Result<bool> {
    let Some(bytes) = serialized_if_over(value, max_inline_bytes)? else {
        return Ok(false);
    };
    let path = materialize_value_bytes(&bytes, run_store, env, run_dir, locality).await?;
    *value = large_value_marker(&path, bytes.len(), &rendered_head(value, &bytes));
    Ok(true)
}

/// Write `bytes` to the sandbox blob file for their content hash and return
/// the file's path.
///
/// Content addressing makes an existing file authoritative, so a value that
/// was already materialized — the common case, since demotion re-runs before
/// every node over copies that are dropped after the preamble is built —
/// costs one existence probe and nothing else. First touch also persists the
/// blob in `run_store`, keeping the file recoverable through the managed
/// blob-reference machinery.
async fn materialize_value_bytes(
    bytes: &[u8],
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
    locality: &mut SandboxLocality,
) -> Result<String> {
    let blob_hash = BlobHash::new(bytes);
    if locality.is_local(env, run_dir).await? {
        let path = local_materialized_blob_path(run_dir, &blob_hash);
        if !path.exists() {
            persist_blob(bytes, run_store).await?;
            write_local_blob_file(&path, bytes).await?;
        }
        return Ok(path.display().to_string());
    }

    let remote_path = format!("{}/.fabro/blobs/{blob_hash}.json", env.working_directory());
    if !env
        .file_exists(&remote_path)
        .await
        .map_err(|e| Error::engine_with_source("failed to check blob existence", e))?
    {
        persist_blob(bytes, run_store).await?;
        let content = std::str::from_utf8(bytes)
            .map_err(|e| Error::engine_with_source("artifact blob was not valid UTF-8 JSON", e))?;
        env.write_file(&remote_path, content).await.map_err(|e| {
            Error::engine_with_source("failed to write artifact blob to sandbox", e)
        })?;
    }
    Ok(remote_path)
}

async fn persist_blob(bytes: &[u8], run_store: &RunStoreHandle) -> Result<()> {
    run_store
        .write_blob(bytes)
        .await
        .map_err(|e| Error::engine_with_anyhow("artifact blob write failed", e))?;
    Ok(())
}

/// Head of the value as the preamble would have rendered it: the raw text for
/// strings, compact JSON otherwise.
fn rendered_head(value: &Value, serialized: &[u8]) -> String {
    if let Some(text) = value.as_str() {
        return text.chars().take(LARGE_VALUE_PREVIEW_CHARS).collect();
    }
    // Four bytes covers the widest UTF-8 character, so this slice always
    // holds at least LARGE_VALUE_PREVIEW_CHARS characters of the rendering.
    let head = &serialized[..serialized.len().min(LARGE_VALUE_PREVIEW_CHARS * 4)];
    String::from_utf8_lossy(head)
        .chars()
        .take(LARGE_VALUE_PREVIEW_CHARS)
        .collect()
}

fn large_value_marker(path: &str, bytes: usize, preview: &str) -> Value {
    serde_json::json!({
        "fabroLargeValue": {
            "bytes": bytes,
            "path": path,
            "hint": LARGE_VALUE_HINT,
            "preview": preview,
        }
    })
}

/// Read the prompt-facing fields from a marker created by
/// [`demote_large_values_for_prompt`] or [`demote_large_items_for_prompt`].
#[must_use]
pub(crate) fn prompt_large_value(value: &Value) -> Option<PromptLargeValue<'_>> {
    let marker = value.get(LARGE_VALUE_MARKER_KEY)?.as_object()?;
    if marker.get("hint")?.as_str()? != LARGE_VALUE_HINT {
        return None;
    }
    Some(PromptLargeValue {
        bytes:   marker.get("bytes")?.as_u64()?,
        path:    marker.get("path")?.as_str()?,
        preview: marker.get("preview")?.as_str()?,
    })
}

fn format_prompt_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
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
    strip_transient_keys(&mut snapshot);
    normalize_durable_updates(&mut snapshot);
    snapshot
}

/// Remove runtime-only keys that must never reach durable storage.
pub(crate) fn strip_transient_keys(values: &mut HashMap<String, Value>) {
    for key in context::keys::TRANSIENT_CONTEXT_KEYS {
        values.remove(*key);
    }
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
    strip_transient_keys(&mut checkpoint.context_values);
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

pub async fn resolve_context_for_edge_selection(
    context: &Context,
    run_store: &RunStoreHandle,
) -> Result<Context> {
    let mut values = context.snapshot();
    for key in [context::keys::COMMAND_OUTPUT] {
        if let Some(Value::String(current)) = values.get_mut(key) {
            *current = resolve_text_or_blob_ref_str(current, run_store).await?;
        }
    }
    Ok(Context::from_values(values))
}

pub async fn resolve_outcomes_for_execution(
    node_outcomes: &HashMap<String, Outcome>,
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
) -> Result<HashMap<String, Outcome>> {
    let mut resolved = node_outcomes.clone();
    let mut locality = SandboxLocality::default();
    for outcome in resolved.values_mut() {
        resolve_execution_values(
            &mut outcome.context_updates,
            run_store,
            env,
            run_dir,
            &mut locality,
        )
        .await?;
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
    let mut locality = SandboxLocality::default();
    resolve_execution_values(&mut values, run_store, env, run_dir, &mut locality).await?;
    Ok(values)
}

pub async fn resolve_text_or_blob_ref(value: &Value, run_store: &RunStoreHandle) -> Result<String> {
    match value.as_str() {
        Some(current) => resolve_text_or_blob_ref_str(current, run_store).await,
        None => Ok(value.to_string()),
    }
}

/// Resolve a structured JSON value from inline context or a Fabro-managed
/// blob reference.
///
/// Managed `file://` references are normalized through their content-addressed
/// blob hash instead of reading an execution-local path. Ordinary strings and
/// ordinary file references remain unchanged for the caller to validate.
pub(crate) async fn resolve_json_value(value: Value, run_store: &RunStoreHandle) -> Result<Value> {
    let blob_hash = value.as_str().and_then(|reference| {
        parse_blob_ref(reference).or_else(|| parse_managed_blob_file_ref(reference))
    });
    let Some(blob_hash) = blob_hash else {
        return Ok(value);
    };

    let bytes = read_required_blob(&blob_hash, run_store).await?;
    serde_json::from_slice(&bytes)
        .map_err(|err| Error::engine_with_source("artifact blob was not valid JSON", err))
}

/// Resolve a flat workflow context key (`context.NAME` or `NAME`) to a
/// hydrated JSON value.
///
/// Returns `Ok(None)` when the key is absent from the context, and `Err` when
/// the value exists but its blob reference could not be hydrated.
pub(crate) async fn resolve_flat_context_value(
    context: &Context,
    key: &str,
    run_store: &RunStoreHandle,
) -> Result<Option<Value>> {
    let Some(value) = context::lookup_flat(context, key) else {
        return Ok(None);
    };
    resolve_json_value(value, run_store).await.map(Some)
}

pub async fn resolve_text_or_blob_ref_str(
    current: &str,
    run_store: &RunStoreHandle,
) -> Result<String> {
    let Some(blob_hash) = parse_blob_ref(current) else {
        return Ok(current.to_string());
    };
    let bytes = run_store
        .read_blob(&blob_hash)
        .await
        .map_err(|e| Error::engine_with_anyhow("text blob read failed", e))?
        .ok_or_else(|| Error::engine(format!("text blob missing: {blob_hash}")))?;
    serde_json::from_slice::<String>(&bytes)
        .map_err(|e| Error::engine_with_source("text blob was not a JSON string", e))
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
                return Err(Error::engine_with_source(
                    "failed to check artifact existence",
                    e,
                ));
            }
        }

        let content = fs::read_to_string(&local_path).await.map_err(|e| {
            Error::engine_with_source(format!("failed to read local artifact {local_path}"), e)
        })?;

        let filename = std::path::Path::new(&local_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("artifact.json");

        let remote_path = format!("{}/.fabro/artifacts/{filename}", env.working_directory());

        env.write_file(&remote_path, &content)
            .await
            .map_err(|e| Error::engine_with_source("failed to write artifact to remote env", e))?;

        *value = Value::String(format!("{ARTIFACT_POINTER_PREFIX}{remote_path}"));
    }
    Ok(())
}

fn normalize_durable_value(value: &mut Value) {
    match value {
        Value::String(current) => {
            if let Some(blob_hash) = parse_managed_blob_file_ref(current) {
                *current = format_blob_ref(&blob_hash);
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
    locality: &'a mut SandboxLocality,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        for (key, value) in values.iter_mut() {
            resolve_execution_value(Some(key.as_str()), value, run_store, env, run_dir, locality)
                .await?;
        }
        Ok(())
    })
}

fn is_text_context_key(key: &str) -> bool {
    key == context::keys::COMMAND_OUTPUT || key.starts_with(context::keys::RESPONSE_PREFIX)
}

fn resolve_execution_value<'a>(
    key: Option<&'a str>,
    value: &'a mut Value,
    run_store: &'a RunStoreHandle,
    env: &'a dyn Sandbox,
    run_dir: &'a Path,
    locality: &'a mut SandboxLocality,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        match value {
            Value::String(current) => {
                if key.is_some_and(is_text_context_key) {
                    *current = resolve_text_or_blob_ref_str(current, run_store).await?;
                } else if let Some(blob_hash) = parse_blob_ref(current) {
                    *current =
                        materialize_blob_ref(&blob_hash, run_store, env, run_dir, locality).await?;
                } else if current.starts_with(ARTIFACT_POINTER_PREFIX)
                    && parse_managed_blob_file_ref(current).is_none()
                {
                    *current = resolve_explicit_file_ref(current, env).await?;
                }
            }
            Value::Array(items) => {
                for item in items {
                    resolve_execution_value(key, item, run_store, env, run_dir, locality).await?;
                }
            }
            Value::Object(map) => {
                for (child_key, item) in map.iter_mut() {
                    let child_context_key = if key.is_some_and(is_text_context_key) {
                        key
                    } else {
                        Some(child_key.as_str())
                    };
                    resolve_execution_value(
                        child_context_key,
                        item,
                        run_store,
                        env,
                        run_dir,
                        locality,
                    )
                    .await?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    })
}

async fn materialize_blob_ref(
    blob_hash: &BlobHash,
    run_store: &RunStoreHandle,
    env: &dyn Sandbox,
    run_dir: &Path,
    locality: &mut SandboxLocality,
) -> Result<String> {
    // Blobs are content-addressed, so an existing materialized file is always
    // current — check before paying for the store read.
    if locality.is_local(env, run_dir).await? {
        let path = local_materialized_blob_path(run_dir, blob_hash);
        if !path.exists() {
            let bytes = read_required_blob(blob_hash, run_store).await?;
            write_local_blob_file(&path, &bytes).await?;
        }
        return Ok(format!("{ARTIFACT_POINTER_PREFIX}{}", path.display()));
    }

    let remote_path = format!("{}/.fabro/blobs/{blob_hash}.json", env.working_directory());
    if !env
        .file_exists(&remote_path)
        .await
        .map_err(|e| Error::engine_with_source("failed to check blob existence", e))?
    {
        let bytes = read_required_blob(blob_hash, run_store).await?;
        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| Error::engine_with_source("artifact blob was not valid UTF-8 JSON", e))?;
        env.write_file(&remote_path, &content).await.map_err(|e| {
            Error::engine_with_source("failed to write artifact blob to sandbox", e)
        })?;
    }

    Ok(format!("{ARTIFACT_POINTER_PREFIX}{remote_path}"))
}

async fn write_local_blob_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.map_err(|err| {
            Error::Io(format!(
                "creating artifact blob directory {}: {err}",
                parent.display()
            ))
        })?;
    }
    fs::write(path, bytes)
        .await
        .map_err(|err| Error::Io(format!("writing artifact blob {}: {err}", path.display())))
}

async fn read_required_blob(
    blob_hash: &BlobHash,
    run_store: &RunStoreHandle,
) -> Result<bytes::Bytes> {
    run_store
        .read_blob(blob_hash)
        .await
        .map_err(|e| Error::engine_with_anyhow("artifact blob read failed", e))?
        .ok_or_else(|| Error::engine(format!("artifact blob missing: {blob_hash}")))
}

async fn resolve_explicit_file_ref(value: &str, env: &dyn Sandbox) -> Result<String> {
    let local_path = value
        .strip_prefix(ARTIFACT_POINTER_PREFIX)
        .ok_or_else(|| Error::engine(format!("invalid artifact pointer: {value}")))?;

    if env
        .file_exists(local_path)
        .await
        .map_err(|e| Error::engine_with_source("failed to check artifact existence", e))?
    {
        return Ok(value.to_string());
    }

    let content = fs::read_to_string(local_path).await.map_err(|e| {
        Error::engine_with_source(format!("failed to read local artifact {local_path}"), e)
    })?;
    let filename = Path::new(local_path)
        .file_name()
        .and_then(|file| file.to_str())
        .unwrap_or("artifact.json");
    let remote_path = format!("{}/.fabro/artifacts/{filename}", env.working_directory());

    if !env
        .file_exists(&remote_path)
        .await
        .map_err(|e| Error::engine_with_source("failed to check artifact existence", e))?
    {
        env.write_file(&remote_path, &content)
            .await
            .map_err(|e| Error::engine_with_source("failed to write artifact to remote env", e))?;
    }

    Ok(format!("{ARTIFACT_POINTER_PREFIX}{remote_path}"))
}

/// Memoized sandbox locality for one resolution pass. The sandbox and run
/// directory are invariant across a pass, so the (possibly remote) probe is
/// paid at most once instead of once per blob reference.
#[derive(Default)]
struct SandboxLocality {
    cached: Option<bool>,
}

impl SandboxLocality {
    async fn is_local(&mut self, env: &dyn Sandbox, run_dir: &Path) -> Result<bool> {
        if let Some(local) = self.cached {
            return Ok(local);
        }
        let local = env
            .file_exists(&run_dir.to_string_lossy())
            .await
            .map_err(|e| Error::engine_with_source("failed to inspect sandbox locality", e))?;
        self.cached = Some(local);
        Ok(local)
    }
}

fn local_materialized_blob_path(run_dir: &Path, blob_hash: &BlobHash) -> PathBuf {
    RunScratch::new(run_dir)
        .runtime_dir()
        .join("blobs")
        .join(format!("{blob_hash}.json"))
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
        let store = fabro_store::test_support::test_database(
            object_store,
            "runs/",
            Duration::from_millis(1),
            None,
        );
        store.create_run(&test_run_id(label)).await.unwrap()
    }

    #[tokio::test]
    async fn offload_replaces_large_values_with_blob_backed_pointer() {
        let run_store = make_run_store("artifact-offload").await;

        let large_string = "x".repeat(BLOB_OFFLOAD_THRESHOLD + 1);
        let serialized = serde_json::to_vec(&serde_json::json!(large_string.clone())).unwrap();
        let expected_blob_hash = fabro_types::BlobHash::new(&serialized);

        let mut updates = HashMap::new();
        updates.insert("response.plan".to_string(), serde_json::json!(large_string));

        offload_large_values(&mut updates, &run_store.clone().into())
            .await
            .unwrap();

        let pointer = updates.get("response.plan").unwrap();
        assert_eq!(
            pointer,
            &serde_json::json!(fabro_types::format_blob_ref(&expected_blob_hash))
        );

        let blob = run_store
            .read_blob(&expected_blob_hash)
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

    #[tokio::test]
    async fn resolve_json_value_hydrates_blob_and_managed_file_references() {
        let run_store = make_run_store("structured-json-resolution").await;
        let value = serde_json::json!([{"name": "api"}, {"name": "web"}]);
        let blob_hash = run_store
            .write_blob(&serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();
        let handle = run_store.clone().into();

        assert_eq!(
            resolve_json_value(serde_json::json!(format_blob_ref(&blob_hash)), &handle)
                .await
                .unwrap(),
            value
        );
        assert_eq!(
            resolve_json_value(
                serde_json::json!(format!("file:///sandbox/.fabro/blobs/{blob_hash}.json")),
                &handle,
            )
            .await
            .unwrap(),
            value
        );
    }

    #[tokio::test]
    async fn resolve_json_value_preserves_inline_json() {
        let run_store = make_run_store("inline-json-resolution").await;
        let value = serde_json::json!([1, 2, 3]);

        assert_eq!(
            resolve_json_value(value.clone(), &run_store.into())
                .await
                .unwrap(),
            value
        );
    }

    #[tokio::test]
    async fn offload_preserves_parallel_results_and_replaces_large_context_updates() {
        let run_store = make_run_store("parallel-result-artifact-offload").await;
        let large_response = "r".repeat(BLOB_OFFLOAD_THRESHOLD + 1);
        let large_output = "o".repeat(BLOB_OFFLOAD_THRESHOLD + 1);
        let large_report = Value::Array(vec![
            Value::String("small".to_string());
            BLOB_OFFLOAD_THRESHOLD / 4
        ]);
        let expected_report_blob = BlobHash::new(&serde_json::to_vec(&large_report).unwrap());
        let mut typed_results = vec![ParallelBranchResult {
            id:              "branch_a".to_string(),
            index:           Some(0),
            item_label:      None,
            status:          fabro_types::StageOutcome::Succeeded,
            context_updates: std::collections::BTreeMap::from([
                (
                    "response.branch_a".to_string(),
                    serde_json::json!(large_response),
                ),
                (
                    context::keys::COMMAND_OUTPUT.to_string(),
                    serde_json::json!(large_output),
                ),
                ("report".to_string(), large_report.clone()),
                ("small".to_string(), serde_json::json!("kept inline")),
            ]),
        }];

        offload_parallel_branch_updates(&mut typed_results, &run_store.clone().into())
            .await
            .unwrap();
        let mut updates = HashMap::from([(
            context::keys::PARALLEL_RESULTS.to_string(),
            serde_json::to_value(typed_results).unwrap(),
        )]);

        // The ordinary lifecycle pass must preserve the typed result structure
        // and the values already offloaded before the completion event.
        offload_large_values(&mut updates, &run_store.clone().into())
            .await
            .unwrap();

        let results = updates[context::keys::PARALLEL_RESULTS]
            .as_array()
            .expect("parallel.results must remain a structured array");
        let branch_updates = results[0]["context_updates"]
            .as_object()
            .expect("context_updates must remain a structured object");
        assert!(
            branch_updates["response.branch_a"]
                .as_str()
                .is_some_and(|value| fabro_types::parse_blob_ref(value).is_some())
        );
        assert!(
            branch_updates[context::keys::COMMAND_OUTPUT]
                .as_str()
                .is_some_and(|value| fabro_types::parse_blob_ref(value).is_some())
        );
        assert_eq!(
            branch_updates["report"],
            serde_json::json!(format_blob_ref(&expected_report_blob))
        );
        let stored_report = run_store
            .read_blob(&expected_report_blob)
            .await
            .unwrap()
            .expect("structured report blob should exist");
        assert_eq!(
            serde_json::from_slice::<Value>(&stored_report).unwrap(),
            large_report
        );
        assert_eq!(branch_updates["small"], serde_json::json!("kept inline"));
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

    #[tokio::test]
    async fn resolve_context_hydrates_nested_parallel_text_blob_references() {
        let run_store = make_run_store("parallel-result-text-resolution").await;
        let response = "full branch response";
        let output = "full command output";
        let response_blob = run_store
            .write_blob(&serde_json::to_vec(response).unwrap())
            .await
            .unwrap();
        let output_blob = run_store
            .write_blob(&serde_json::to_vec(output).unwrap())
            .await
            .unwrap();
        let unrelated_blob = run_store
            .write_blob(&serde_json::to_vec("unrelated artifact").unwrap())
            .await
            .unwrap();
        let context = Context::new();
        context.set(
            context::keys::PARALLEL_RESULTS,
            serde_json::json!([{
                "id": "branch_a",
                "status": "succeeded",
                "context_updates": {
                    "response.branch_a": fabro_types::format_blob_ref(&response_blob),
                    "response.nested": {
                        "text": fabro_types::format_blob_ref(&response_blob),
                        "items": [fabro_types::format_blob_ref(&output_blob)],
                    },
                    "command.output": fabro_types::format_blob_ref(&output_blob),
                    "report": fabro_types::format_blob_ref(&unrelated_blob),
                }
            }]),
        );
        let env = TestSyncEnv::new(true, "/workspace");
        let run_dir = tempfile::tempdir().unwrap();

        let resolved =
            resolved_context_snapshot(&context, &run_store.clone().into(), &env, run_dir.path())
                .await
                .unwrap();

        let updates = &resolved[context::keys::PARALLEL_RESULTS][0]["context_updates"];
        assert_eq!(updates["response.branch_a"], serde_json::json!(response));
        assert_eq!(
            updates["response.nested"]["text"],
            serde_json::json!(response)
        );
        assert_eq!(
            updates["response.nested"]["items"][0],
            serde_json::json!(output)
        );
        assert_eq!(
            updates[context::keys::COMMAND_OUTPUT],
            serde_json::json!(output)
        );
        assert!(
            updates["report"]
                .as_str()
                .is_some_and(|value| value.starts_with("file://")),
            "non-textual nested values should retain artifact semantics"
        );
    }

    #[tokio::test]
    async fn resolve_context_probes_sandbox_locality_once_per_pass() {
        let run_store = make_run_store("locality-probe-memoization").await;
        let first_blob = run_store
            .write_blob(&serde_json::to_vec(&serde_json::json!({"a": 1})).unwrap())
            .await
            .unwrap();
        let second_blob = run_store
            .write_blob(&serde_json::to_vec(&serde_json::json!({"b": 2})).unwrap())
            .await
            .unwrap();
        let context = Context::new();
        context.set("first", fabro_types::format_blob_ref(&first_blob).into());
        context.set("second", fabro_types::format_blob_ref(&second_blob).into());
        let env = TestSyncEnv::new(true, "/workspace");
        let run_dir = tempfile::tempdir().unwrap();

        resolved_context_snapshot(&context, &run_store.clone().into(), &env, run_dir.path())
            .await
            .unwrap();

        assert_eq!(
            *env.exists_calls.lock().unwrap(),
            1,
            "sandbox locality should be probed once per resolution pass"
        );
    }

    #[test]
    fn normalize_durable_updates_rewrites_managed_blob_file_refs_recursively() {
        let blob_hash = fabro_types::BlobHash::new(b"hello");
        let mut updates = HashMap::from([(
            "nested".to_string(),
            serde_json::json!({
                "items": [
                    format!("file:///tmp/run/runtime/blobs/{blob_hash}.json"),
                    format!("file:///sandbox/.fabro/blobs/{blob_hash}.json"),
                    "file:///tmp/report.json",
                ]
            }),
        )]);

        normalize_durable_updates(&mut updates);

        assert_eq!(
            updates["nested"],
            serde_json::json!({
                "items": [
                    fabro_types::format_blob_ref(&blob_hash),
                    fabro_types::format_blob_ref(&blob_hash),
                    "file:///tmp/report.json",
                ]
            })
        );
    }

    #[test]
    fn durable_context_snapshot_drops_parallel_branch_preambles() {
        let context = Context::new();
        context.set(
            context::keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES,
            serde_json::json!({"branch-a": "runtime only"}),
        );
        context.set("response.work", serde_json::json!("durable"));

        let snapshot = durable_context_snapshot(&context);

        assert!(!snapshot.contains_key(context::keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES));
        assert_eq!(
            snapshot.get("response.work"),
            Some(&serde_json::json!("durable"))
        );
    }

    #[test]
    fn normalize_checkpoint_for_resume_drops_parallel_branch_preambles() {
        let mut checkpoint = crate::records::Checkpoint {
            timestamp:                  chrono::Utc::now(),
            current_node:               "work".to_string(),
            completed_nodes:            vec!["work".to_string()],
            node_retries:               HashMap::new(),
            context_values:             HashMap::from([
                (
                    context::keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES.to_string(),
                    serde_json::json!({"branch-a": "runtime only"}),
                ),
                ("response.work".to_string(), serde_json::json!("durable")),
            ]),
            node_outcomes:              HashMap::new(),
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
                .contains_key(context::keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES)
        );
        assert_eq!(
            checkpoint.context_values.get("response.work"),
            Some(&serde_json::json!("durable"))
        );
    }

    #[test]
    fn normalize_checkpoint_for_resume_converts_managed_blob_file_refs_and_drops_preamble() {
        let blob_hash = fabro_types::BlobHash::new(b"managed");
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
                    serde_json::json!(format!("file:///sandbox/.fabro/blobs/{blob_hash}.json")),
                ),
            ]),
            node_outcomes:              HashMap::from([(
                "work".to_string(),
                crate::outcome::Outcome {
                    context_updates: HashMap::from([(
                        "response.work".to_string(),
                        serde_json::json!(format!("file:///sandbox/.fabro/blobs/{blob_hash}.json")),
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
            Some(&serde_json::json!(fabro_types::format_blob_ref(&blob_hash)))
        );
        assert_eq!(
            checkpoint
                .node_outcomes
                .get("work")
                .and_then(|outcome| outcome.context_updates.get("response.work")),
            Some(&serde_json::json!(fabro_types::format_blob_ref(&blob_hash)))
        );
    }

    // --- sync_artifacts_to_env tests ---

    use std::sync::Mutex;

    struct TestSyncEnv {
        accessible:   bool,
        written:      Mutex<Vec<(String, String)>>,
        working_dir:  String,
        exists_calls: Mutex<usize>,
    }

    impl TestSyncEnv {
        fn new(accessible: bool, working_dir: &str) -> Self {
            Self {
                accessible,
                written: Mutex::new(Vec::new()),
                working_dir: working_dir.to_string(),
                exists_calls: Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Sandbox for TestSyncEnv {
        async fn read_file_bytes(&self, _path: &str) -> fabro_sandbox::Result<Vec<u8>> {
            Err("not implemented".into())
        }

        async fn write_file(&self, path: &str, content: &str) -> fabro_sandbox::Result<()> {
            self.written
                .lock()
                .unwrap()
                .push((path.to_string(), content.to_string()));
            Ok(())
        }

        async fn delete_file(&self, _path: &str) -> fabro_sandbox::Result<()> {
            Err("not implemented".into())
        }

        async fn file_exists(&self, _path: &str) -> fabro_sandbox::Result<bool> {
            *self.exists_calls.lock().unwrap() += 1;
            Ok(self.accessible)
        }

        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> fabro_sandbox::Result<Vec<fabro_agent::DirEntry>> {
            Err("not implemented".into())
        }

        async fn exec_command(
            &self,
            _command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            _cancel_token: Option<tokio_util::sync::CancellationToken>,
        ) -> fabro_sandbox::Result<fabro_agent::ExecResult> {
            Err("not implemented".into())
        }

        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &fabro_agent::GrepOptions,
        ) -> fabro_sandbox::Result<Vec<String>> {
            Err("not implemented".into())
        }

        async fn glob(
            &self,
            _pattern: &str,
            _path: Option<&str>,
        ) -> fabro_sandbox::Result<Vec<String>> {
            Err("not implemented".into())
        }

        async fn download_file_to_local(
            &self,
            _remote_path: &str,
            _local_path: &std::path::Path,
        ) -> fabro_sandbox::Result<()> {
            Err("not implemented".into())
        }

        async fn upload_file_from_local(
            &self,
            _local_path: &std::path::Path,
            _remote_path: &str,
        ) -> fabro_sandbox::Result<()> {
            Err("not implemented".into())
        }

        async fn initialize(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }

        async fn cleanup(&self) -> fabro_sandbox::Result<()> {
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

    #[tokio::test]
    async fn demote_replaces_oversized_prompt_values_with_preview_markers() {
        let run_store: RunStoreHandle = make_run_store("prompt-demote").await.into();
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let sandbox = fabro_agent::LocalSandbox::new(tmp.path().to_path_buf());

        let dataset = serde_json::json!({
            "rows": vec![serde_json::json!({"payload": "x".repeat(64)}); 256]
        });
        let mut values = HashMap::from([
            ("dataset".to_string(), dataset.clone()),
            ("small".to_string(), serde_json::json!("kept inline")),
        ]);
        let mut outcomes = HashMap::from([("work".to_string(), Outcome {
            context_updates: HashMap::from([(
                context::keys::COMMAND_OUTPUT.to_string(),
                serde_json::json!("o".repeat(PROMPT_INLINE_VALUE_MAX + 1)),
            )]),
            ..Outcome::success()
        })]);
        let mut selection = PromptValueSelection::default();
        selection.select_context_value("dataset");
        selection.select_context_value("small");
        selection.select_outcome_value("work", context::keys::COMMAND_OUTPUT);

        demote_large_values_for_prompt(
            &mut values,
            &mut outcomes,
            &selection,
            &run_store,
            &sandbox,
            &run_dir,
        )
        .await;

        let details = values["dataset"]
            .get("fabroLargeValue")
            .expect("oversized context value should demote");
        assert_eq!(
            usize::try_from(details["bytes"].as_u64().unwrap()).unwrap(),
            serde_json::to_vec(&dataset).unwrap().len()
        );
        let stored: Value =
            serde_json::from_slice(&std::fs::read(details["path"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(stored, dataset);
        assert!(
            details["preview"]
                .as_str()
                .unwrap()
                .starts_with("{\"rows\"")
        );
        assert_eq!(
            details["preview"].as_str().unwrap().chars().count(),
            LARGE_VALUE_PREVIEW_CHARS
        );
        assert!(serde_json::to_vec(&values["dataset"]).unwrap().len() <= PROMPT_INLINE_VALUE_MAX);

        assert_eq!(values["small"], serde_json::json!("kept inline"));

        let details = outcomes["work"].context_updates[context::keys::COMMAND_OUTPUT]
            .get("fabroLargeValue")
            .expect("oversized command output should demote");
        assert!(details["preview"].as_str().unwrap().starts_with("ooo"));
    }

    #[tokio::test]
    async fn demote_skips_values_the_selection_omits() {
        let run_store: RunStoreHandle = make_run_store("prompt-demote-unselected").await.into();
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let sandbox = fabro_agent::LocalSandbox::new(tmp.path().to_path_buf());

        let unrendered_context = "p".repeat(PROMPT_INLINE_VALUE_MAX + 1);
        let unrendered_response = "r".repeat(PROMPT_INLINE_VALUE_MAX + 1);
        let mut values = HashMap::from([(
            context::keys::CURRENT_PREAMBLE.to_string(),
            serde_json::json!(unrendered_context.clone()),
        )]);
        let mut outcomes = HashMap::from([("work".to_string(), Outcome {
            context_updates: HashMap::from([(
                "response.work".to_string(),
                serde_json::json!(unrendered_response.clone()),
            )]),
            ..Outcome::success()
        })]);

        demote_large_values_for_prompt(
            &mut values,
            &mut outcomes,
            &PromptValueSelection::default(),
            &run_store,
            &sandbox,
            &run_dir,
        )
        .await;

        assert_eq!(
            values[context::keys::CURRENT_PREAMBLE],
            serde_json::json!(unrendered_context)
        );
        assert_eq!(
            outcomes["work"].context_updates["response.work"],
            serde_json::json!(unrendered_response)
        );
        let blobs_dir = RunScratch::new(&run_dir).runtime_dir().join("blobs");
        assert!(
            !blobs_dir.exists(),
            "no blob file may be materialized for an unselected value"
        );
    }

    #[test]
    fn prompt_value_selection_merges_and_reports_emptiness() {
        let mut selection = PromptValueSelection::default();
        assert!(selection.is_empty());

        let mut other = PromptValueSelection::default();
        other.select_context_value("output.plan");
        other.select_outcome_value("run_cmd", context::keys::COMMAND_OUTPUT);
        selection.merge(other);

        assert!(!selection.is_empty());
        assert!(selection.renders_context_value("output.plan"));
        assert!(!selection.renders_context_value("response.plan"));
        assert!(selection.renders_outcome_value("run_cmd", context::keys::COMMAND_OUTPUT));
        assert!(!selection.renders_outcome_value("other", context::keys::COMMAND_OUTPUT));
    }
}
