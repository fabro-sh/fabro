use std::collections::HashMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use fabro_store::{EventEnvelope, RunProjection, StageId};
use fabro_types::{RunBlobId, parse_blob_ref, parse_legacy_blob_file_ref};
use futures::future::BoxFuture;

use crate::git::MetadataStore;

#[derive(Debug, Clone)]
pub struct RunDump {
    entries: Vec<RunDumpEntry>,
}

#[derive(Debug, Clone)]
pub struct RunDumpEntry {
    path: String,
    contents: RunDumpContents,
}

#[derive(Debug, Clone)]
pub enum RunDumpContents {
    Text(String),
    Json(serde_json::Value),
    Bytes(Vec<u8>),
}

impl RunDump {
    #[must_use]
    pub fn metadata_init(state: &RunProjection) -> Self {
        let mut entries = Vec::new();
        if let Some(record) = state.run.as_ref() {
            push_json_entry(&mut entries, "run.json", record);
        }
        if let Some(record) = state.start.as_ref() {
            push_json_entry(&mut entries, "start.json", record);
        }
        if let Some(record) = state.sandbox.as_ref() {
            push_json_entry(&mut entries, "sandbox.json", record);
        }
        Self { entries }
    }

    #[must_use]
    pub fn metadata_checkpoint(state: &RunProjection) -> Self {
        let mut entries = Vec::new();
        let mut keys: Vec<_> = state.iter_nodes().map(|(node, _)| node.clone()).collect();
        keys.sort();

        for node_key in keys {
            let Some(node) = state.node(&node_key) else {
                continue;
            };
            let node_id = node_key.node_id();
            let visit = node_key.visit();

            if let Some(prompt) = node.prompt.as_ref() {
                entries.push(RunDumpEntry::text(
                    metadata_node_file_path(node_id, visit, "prompt.md"),
                    prompt.clone(),
                ));
            }
            if let Some(response) = node.response.as_ref() {
                entries.push(RunDumpEntry::text(
                    metadata_node_file_path(node_id, visit, "response.md"),
                    response.clone(),
                ));
            }
            if let Some(status) = node.status.as_ref() {
                push_json_entry_path(
                    &mut entries,
                    &PathBuf::from(metadata_node_file_path(node_id, visit, "status.json")),
                    status,
                );
            }
            if let Some(provider_used) = node.provider_used.as_ref() {
                entries.push(RunDumpEntry::json(
                    metadata_node_file_path(node_id, visit, "provider_used.json"),
                    provider_used.clone(),
                ));
            }
            if let Some(diff) = node.diff.as_ref() {
                entries.push(RunDumpEntry::text(
                    metadata_node_file_path(node_id, visit, "diff.patch"),
                    diff.clone(),
                ));
            }
            if let Some(script_invocation) = node.script_invocation.as_ref() {
                entries.push(RunDumpEntry::json(
                    metadata_node_file_path(node_id, visit, "script_invocation.json"),
                    script_invocation.clone(),
                ));
            }
            if let Some(script_timing) = node.script_timing.as_ref() {
                entries.push(RunDumpEntry::json(
                    metadata_node_file_path(node_id, visit, "script_timing.json"),
                    script_timing.clone(),
                ));
            }
            if let Some(parallel_results) = node.parallel_results.as_ref() {
                entries.push(RunDumpEntry::json(
                    metadata_node_file_path(node_id, visit, "parallel_results.json"),
                    parallel_results.clone(),
                ));
            }
        }

        Self { entries }
    }

    #[must_use]
    pub fn metadata_finalize(state: &RunProjection) -> Self {
        let mut dump = Self::metadata_checkpoint(state);
        if let Some(retro) = state.retro.as_ref() {
            push_json_entry(&mut dump.entries, "retro.json", retro);
        }
        dump
    }

    pub fn from_store_state_and_events(
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
            entries.push(RunDumpEntry::text("graph.fabro", graph_source.clone()));
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
                entries.push(RunDumpEntry::text_path(
                    &base.join("prompt.md"),
                    prompt.clone(),
                ));
            }
            if let Some(response) = node.response.as_ref() {
                entries.push(RunDumpEntry::text_path(
                    &base.join("response.md"),
                    response.clone(),
                ));
            }
            if let Some(status) = node.status.as_ref() {
                push_json_entry_path(&mut entries, &base.join("status.json"), status);
            }
            if let Some(stdout) = node.stdout.as_ref() {
                entries.push(RunDumpEntry::text_path(
                    &base.join("stdout.log"),
                    stdout.clone(),
                ));
            }
            if let Some(stderr) = node.stderr.as_ref() {
                entries.push(RunDumpEntry::text_path(
                    &base.join("stderr.log"),
                    stderr.clone(),
                ));
            }
        }

        if let Some(prompt) = state.retro_prompt.as_ref() {
            entries.push(RunDumpEntry::text("retro/prompt.md", prompt.clone()));
        }
        if let Some(response) = state.retro_response.as_ref() {
            entries.push(RunDumpEntry::text("retro/response.md", response.clone()));
        }

        let mut events_jsonl = Vec::new();
        for event in events {
            serde_json::to_writer(&mut events_jsonl, event)?;
            events_jsonl.write_all(b"\n")?;
        }
        entries.push(RunDumpEntry::bytes("events.jsonl", events_jsonl));

        for (seq, checkpoint) in &state.checkpoints {
            push_json_entry_path(
                &mut entries,
                &PathBuf::from("checkpoints").join(format!("{seq:04}.json")),
                checkpoint,
            );
        }

        Ok(Self { entries })
    }

    pub fn add_artifact_bytes(
        &mut self,
        stage_id: &StageId,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<()> {
        let path = artifact_dump_path(stage_id, filename)?;
        self.entries.push(RunDumpEntry::bytes_path(&path, data));
        Ok(())
    }

    pub async fn hydrate_referenced_blobs_with_reader<'a, F>(
        &mut self,
        mut read_blob: F,
    ) -> Result<()>
    where
        F: FnMut(RunBlobId) -> BoxFuture<'a, Result<Option<Bytes>>>,
    {
        let mut cache = HashMap::new();
        for entry in &mut self.entries {
            if let RunDumpContents::Json(value) = &mut entry.contents {
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

    pub fn entries(&self) -> &[RunDumpEntry] {
        &self.entries
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.entries.len()
    }

    pub fn write_to_dir(&self, root: &Path) -> Result<usize> {
        for entry in &self.entries {
            entry.write_to_dir(root)?;
        }
        Ok(self.file_count())
    }

    pub fn write_to_metadata_store(
        &self,
        store: &MetadataStore,
        run_id: &str,
        message: &str,
    ) -> Result<()> {
        let git_entries = self.git_entries()?;
        let refs: Vec<(&str, &[u8])> = git_entries
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        store.write_files(run_id, &refs, message)?;
        Ok(())
    }

    pub fn git_entries(&self) -> Result<Vec<(String, Vec<u8>)>> {
        self.entries
            .iter()
            .map(|entry| Ok((entry.path.clone(), entry.contents.to_bytes()?)))
            .collect()
    }
}

impl RunDumpEntry {
    fn text(path: impl Into<String>, contents: String) -> Self {
        Self {
            path: path.into(),
            contents: RunDumpContents::Text(contents),
        }
    }

    fn text_path(path: &Path, contents: String) -> Self {
        Self {
            path: path_to_string(path),
            contents: RunDumpContents::Text(contents),
        }
    }

    fn json(path: impl Into<String>, contents: serde_json::Value) -> Self {
        Self {
            path: path.into(),
            contents: RunDumpContents::Json(contents),
        }
    }

    fn json_path(path: &Path, contents: serde_json::Value) -> Self {
        Self {
            path: path_to_string(path),
            contents: RunDumpContents::Json(contents),
        }
    }

    fn bytes(path: impl Into<String>, contents: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            contents: RunDumpContents::Bytes(contents),
        }
    }

    fn bytes_path(path: &Path, contents: Vec<u8>) -> Self {
        Self {
            path: path_to_string(path),
            contents: RunDumpContents::Bytes(contents),
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

impl RunDumpContents {
    fn to_bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Text(value) => Ok(value.as_bytes().to_vec()),
            Self::Json(value) => Ok(serde_json::to_vec_pretty(value)?),
            Self::Bytes(value) => Ok(value.clone()),
        }
    }
}

fn push_json_entry<T>(entries: &mut Vec<RunDumpEntry>, path: &str, value: &T)
where
    T: serde::Serialize,
{
    if let Ok(value) = serde_json::to_value(value) {
        entries.push(RunDumpEntry::json(path, value));
    }
}

fn push_json_entry_path<T>(entries: &mut Vec<RunDumpEntry>, path: &Path, value: &T)
where
    T: serde::Serialize,
{
    if let Ok(value) = serde_json::to_value(value) {
        entries.push(RunDumpEntry::json_path(path, value));
    }
}

fn metadata_node_file_path(node_id: &str, visit: u32, filename: &str) -> String {
    if visit <= 1 {
        format!("nodes/{node_id}/{filename}")
    } else {
        format!("nodes/{node_id}-visit_{visit}/{filename}")
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
