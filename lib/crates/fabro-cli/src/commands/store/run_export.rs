#![expect(
    clippy::disallowed_methods,
    reason = "CLI-owned export writer uses sync std::fs for final local materialization"
)]

use std::collections::HashMap;
#[expect(
    clippy::disallowed_types,
    reason = "in-memory Vec<u8>::write_all for jsonl serialization; no filesystem or network I/O"
)]
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use fabro_store::{EventEnvelope, RunProjection, StageId};
use fabro_types::{RunBlobId, parse_blob_ref, parse_legacy_blob_file_ref};
use futures::future::BoxFuture;

#[derive(Debug, Clone)]
pub(super) struct StoreRunExport {
    entries: Vec<StoreRunExportEntry>,
}

#[derive(Debug, Clone)]
struct StoreRunExportEntry {
    path:     String,
    contents: StoreRunExportContents,
}

#[derive(Debug, Clone)]
enum StoreRunExportContents {
    Text(String),
    Json(serde_json::Value),
    Bytes(Vec<u8>),
}

impl StoreRunExport {
    pub(super) fn from_store_state_and_events(
        state: &RunProjection,
        events: &[EventEnvelope],
    ) -> Result<Self> {
        let mut entries = Vec::new();

        if let Some(record) = state.run.as_ref() {
            push_json_entry(&mut entries, "run.json", record);
        }
        if let Some(record) = state.start.as_ref() {
            push_json_entry(&mut entries, "start.json", record);
        }
        if let Some(record) = state.status.as_ref() {
            push_json_entry(&mut entries, "status.json", record);
        }
        if let Some(record) = state.checkpoint.as_ref() {
            push_json_entry(&mut entries, "checkpoint.json", record);
        }
        if let Some(record) = state.conclusion.as_ref() {
            push_json_entry(&mut entries, "conclusion.json", record);
        }
        if let Some(record) = state.retro.as_ref() {
            push_json_entry(&mut entries, "retro.json", record);
        }
        if let Some(graph_source) = state.graph_source.as_ref() {
            entries.push(StoreRunExportEntry::text(
                "graph.fabro",
                graph_source.clone(),
            ));
        }
        if let Some(record) = state.sandbox.as_ref() {
            push_json_entry(&mut entries, "sandbox.json", record);
        }

        let mut node_keys: Vec<_> = state.iter_nodes().map(|(node, _)| node.clone()).collect();
        node_keys.sort();
        for node_key in &node_keys {
            let node = state
                .node(node_key)
                .with_context(|| format!("missing node {node_key:?} in projection"))?;
            let node_id_segment = validate_single_path_segment("node id", node_key.node_id())?;
            let base = PathBuf::from("nodes")
                .join(node_id_segment)
                .join(format!("visit-{}", node_key.visit()));

            if let Some(prompt) = node.prompt.as_ref() {
                entries.push(StoreRunExportEntry::text_path(
                    &base.join("prompt.md"),
                    prompt.clone(),
                ));
            }
            if let Some(response) = node.response.as_ref() {
                entries.push(StoreRunExportEntry::text_path(
                    &base.join("response.md"),
                    response.clone(),
                ));
            }
            if let Some(status) = node.status.as_ref() {
                push_json_entry_path(&mut entries, &base.join("status.json"), status);
            }
            if let Some(stdout) = node.stdout.as_ref() {
                entries.push(StoreRunExportEntry::text_path(
                    &base.join("stdout.log"),
                    stdout.clone(),
                ));
            }
            if let Some(stderr) = node.stderr.as_ref() {
                entries.push(StoreRunExportEntry::text_path(
                    &base.join("stderr.log"),
                    stderr.clone(),
                ));
            }
        }

        if let Some(prompt) = state.retro_prompt.as_ref() {
            entries.push(StoreRunExportEntry::text("retro/prompt.md", prompt.clone()));
        }
        if let Some(response) = state.retro_response.as_ref() {
            entries.push(StoreRunExportEntry::text(
                "retro/response.md",
                response.clone(),
            ));
        }

        let mut events_jsonl = Vec::new();
        for event in events {
            serde_json::to_writer(&mut events_jsonl, event)?;
            events_jsonl.write_all(b"\n")?;
        }
        entries.push(StoreRunExportEntry::bytes("events.jsonl", events_jsonl));

        for (seq, checkpoint) in &state.checkpoints {
            push_json_entry_path(
                &mut entries,
                &PathBuf::from("checkpoints").join(format!("{seq:04}.json")),
                checkpoint,
            );
        }

        Ok(Self { entries })
    }

    pub(super) fn add_artifact_bytes(
        &mut self,
        stage_id: &StageId,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<()> {
        let path = artifact_dump_path(stage_id, filename)?;
        self.entries
            .push(StoreRunExportEntry::bytes_path(&path, data));
        Ok(())
    }

    pub(super) async fn hydrate_referenced_blobs_with_reader<'a, F>(
        &mut self,
        mut read_blob: F,
    ) -> Result<()>
    where
        F: FnMut(RunBlobId) -> BoxFuture<'a, Result<Option<Bytes>>>,
    {
        let mut cache = HashMap::new();
        for entry in &mut self.entries {
            if let StoreRunExportContents::Json(value) = &mut entry.contents {
                let mut blob_ids = Vec::new();
                collect_blob_refs_in_value(value, &mut blob_ids);
                for blob_id in blob_ids {
                    if cache.contains_key(&blob_id) {
                        continue;
                    }
                    let blob = read_blob(blob_id)
                        .await?
                        .with_context(|| format!("blob {blob_id:?} is missing from the store"))?;
                    let hydrated: serde_json::Value = serde_json::from_slice(&blob)
                        .with_context(|| format!("blob {blob_id:?} is not valid JSON"))?;
                    cache.insert(blob_id, hydrated);
                }
                replace_blob_refs_in_value(value, &cache)?;
            }
        }
        Ok(())
    }

    pub(super) fn write_to_dir(&self, root: &Path) -> Result<usize> {
        for entry in &self.entries {
            entry.write_to_dir(root)?;
        }
        Ok(self.entries.len())
    }
}

impl StoreRunExportEntry {
    fn text(path: impl Into<String>, contents: String) -> Self {
        Self {
            path:     path.into(),
            contents: StoreRunExportContents::Text(contents),
        }
    }

    fn text_path(path: &Path, contents: String) -> Self {
        Self {
            path:     path_to_string(path),
            contents: StoreRunExportContents::Text(contents),
        }
    }

    fn json(path: impl Into<String>, contents: serde_json::Value) -> Self {
        Self {
            path:     path.into(),
            contents: StoreRunExportContents::Json(contents),
        }
    }

    fn json_path(path: &Path, contents: serde_json::Value) -> Self {
        Self {
            path:     path_to_string(path),
            contents: StoreRunExportContents::Json(contents),
        }
    }

    fn bytes(path: impl Into<String>, contents: Vec<u8>) -> Self {
        Self {
            path:     path.into(),
            contents: StoreRunExportContents::Bytes(contents),
        }
    }

    fn bytes_path(path: &Path, contents: Vec<u8>) -> Self {
        Self {
            path:     path_to_string(path),
            contents: StoreRunExportContents::Bytes(contents),
        }
    }

    fn write_to_dir(&self, root: &Path) -> Result<()> {
        let relative = validate_relative_path("run dump path", &self.path)?;
        let path = root.join(relative);
        ensure_parent_dir(&path)?;
        std::fs::write(&path, self.contents.to_bytes()?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

impl StoreRunExportContents {
    fn to_bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Text(value) => Ok(value.as_bytes().to_vec()),
            Self::Json(value) => Ok(serde_json::to_vec_pretty(value)?),
            Self::Bytes(value) => Ok(value.clone()),
        }
    }
}

fn push_json_entry<T>(entries: &mut Vec<StoreRunExportEntry>, path: &str, value: &T)
where
    T: serde::Serialize,
{
    if let Ok(value) = serde_json::to_value(value) {
        entries.push(StoreRunExportEntry::json(path, value));
    }
}

fn push_json_entry_path<T>(entries: &mut Vec<StoreRunExportEntry>, path: &Path, value: &T)
where
    T: serde::Serialize,
{
    if let Ok(value) = serde_json::to_value(value) {
        entries.push(StoreRunExportEntry::json_path(path, value));
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn validate_single_path_segment(kind: &str, value: &str) -> Result<PathBuf> {
    let path = validate_relative_path(kind, value)?;
    if path.components().count() != 1 {
        bail!("{kind} {value:?} must be a single path segment");
    }
    Ok(path)
}

fn validate_relative_path(kind: &str, value: &str) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("{kind} {value:?} must be a relative path without '..'");
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("{kind} {value:?} must not be empty");
    }
    Ok(normalized)
}

fn collect_blob_refs_in_value(value: &serde_json::Value, blob_ids: &mut Vec<RunBlobId>) {
    match value {
        serde_json::Value::String(current) => {
            if let Some(blob_id) =
                parse_blob_ref(current).or_else(|| parse_legacy_blob_file_ref(current))
            {
                blob_ids.push(blob_id);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_blob_refs_in_value(item, blob_ids);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_blob_refs_in_value(item, blob_ids);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn replace_blob_refs_in_value(
    value: &mut serde_json::Value,
    cache: &HashMap<RunBlobId, serde_json::Value>,
) -> Result<()> {
    match value {
        serde_json::Value::String(current) => {
            let Some(blob_id) =
                parse_blob_ref(current).or_else(|| parse_legacy_blob_file_ref(current))
            else {
                return Ok(());
            };
            let hydrated = cache
                .get(&blob_id)
                .cloned()
                .with_context(|| format!("blob {blob_id:?} is missing from the hydration cache"))?;
            *value = hydrated;
        }
        serde_json::Value::Array(items) => {
            for item in items {
                replace_blob_refs_in_value(item, cache)?;
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                replace_blob_refs_in_value(item, cache)?;
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    Ok(())
}

fn artifact_dump_path(stage_id: &StageId, filename: &str) -> Result<PathBuf> {
    let node_id_segment = validate_single_path_segment("node id", stage_id.node_id())?;
    let filename_path = validate_relative_path("artifact filename", filename)?;
    Ok(PathBuf::from("artifacts")
        .join("nodes")
        .join(node_id_segment)
        .join(format!("visit-{}", stage_id.visit()))
        .join(filename_path))
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path {} has no parent", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    Ok(())
}
