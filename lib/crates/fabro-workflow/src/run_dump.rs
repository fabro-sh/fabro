#![expect(
    clippy::disallowed_methods,
    reason = "sync run dump writer used by CLI export paths; async callers wrap it in spawn_blocking"
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
use fabro_store::{EventEnvelope, RunProjection, SerializableProjection, StageId};
use fabro_types::{RunBlobId, parse_blob_ref, parse_legacy_blob_file_ref};
use futures::future::BoxFuture;

#[derive(Debug, Clone)]
pub struct RunDump {
    entries: Vec<RunDumpEntry>,
}

#[derive(Debug, Clone)]
pub struct RunDumpEntry {
    path:     String,
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
    pub fn from_projection(state: &RunProjection) -> Self {
        let mut entries = Vec::new();

        push_json_entry(&mut entries, "run.json", &SerializableProjection(state));

        if let Some(graph_source) = state.graph_source.as_ref() {
            entries.push(RunDumpEntry::text("graph.fabro", graph_source.clone()));
        }

        let mut stage_ids: Vec<_> = state
            .iter_nodes()
            .map(|(stage_id, _)| stage_id.clone())
            .collect();
        stage_ids.sort();

        for stage_id in stage_ids {
            let Some(node) = state.node(&stage_id) else {
                continue;
            };
            let base = PathBuf::from("stages").join(stage_id.to_string());

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
            if let Some(provider_used) = node.provider_used.as_ref() {
                entries.push(RunDumpEntry::json_path(
                    &base.join("provider_used.json"),
                    provider_used.clone(),
                ));
            }
            if let Some(diff) = node.diff.as_ref() {
                entries.push(RunDumpEntry::text_path(
                    &base.join("diff.patch"),
                    diff.clone(),
                ));
            }
            if let Some(script_invocation) = node.script_invocation.as_ref() {
                entries.push(RunDumpEntry::json_path(
                    &base.join("script_invocation.json"),
                    script_invocation.clone(),
                ));
            }
            if let Some(script_timing) = node.script_timing.as_ref() {
                entries.push(RunDumpEntry::json_path(
                    &base.join("script_timing.json"),
                    script_timing.clone(),
                ));
            }
            if let Some(parallel_results) = node.parallel_results.as_ref() {
                entries.push(RunDumpEntry::json_path(
                    &base.join("parallel_results.json"),
                    parallel_results.clone(),
                ));
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
            entries.push(RunDumpEntry::text("stages/retro/prompt.md", prompt.clone()));
        }
        if let Some(response) = state.retro_response.as_ref() {
            entries.push(RunDumpEntry::text(
                "stages/retro/response.md",
                response.clone(),
            ));
        }

        Self { entries }
    }

    pub fn from_store_state_and_events(
        state: &RunProjection,
        events: &[EventEnvelope],
    ) -> Result<Self> {
        let mut dump = Self::from_projection(state);

        let mut events_jsonl = Vec::new();
        for event in events {
            serde_json::to_writer(&mut events_jsonl, event)?;
            events_jsonl.write_all(b"\n")?;
        }
        dump.entries
            .push(RunDumpEntry::bytes("events.jsonl", events_jsonl));

        for (seq, checkpoint) in &state.checkpoints {
            push_json_entry_path(
                &mut dump.entries,
                &PathBuf::from("checkpoints").join(format!("{seq:04}.json")),
                checkpoint,
            );
        }

        Ok(dump)
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

    pub fn add_file_bytes(&mut self, path: impl Into<String>, contents: Vec<u8>) {
        self.entries.push(RunDumpEntry::bytes(path, contents));
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
            match &mut entry.contents {
                RunDumpContents::Json(value) => {
                    let mut blob_ids = Vec::new();
                    collect_blob_refs_in_value(value, &mut blob_ids);
                    for blob_id in blob_ids {
                        if cache.contains_key(&blob_id) {
                            continue;
                        }
                        let blob = read_blob(blob_id).await?.with_context(|| {
                            format!("blob {blob_id:?} is missing from the store")
                        })?;
                        let hydrated: serde_json::Value = serde_json::from_slice(&blob)
                            .with_context(|| format!("blob {blob_id:?} is not valid JSON"))?;
                        cache.insert(blob_id, hydrated);
                    }
                    replace_blob_refs_in_value(value, &cache)?;
                }
                RunDumpContents::Text(text) => {
                    let Some(blob_id) = parse_blob_ref(text) else {
                        continue;
                    };
                    let blob = read_blob(blob_id)
                        .await?
                        .with_context(|| format!("blob {blob_id:?} is missing from the store"))?;
                    *text = serde_json::from_slice::<String>(&blob).with_context(|| {
                        format!("blob {blob_id:?} is not a JSON string text log")
                    })?;
                }
                RunDumpContents::Bytes(_) => {}
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
            path:     path.into(),
            contents: RunDumpContents::Text(contents),
        }
    }

    fn text_path(path: &Path, contents: String) -> Self {
        Self {
            path:     path_to_string(path),
            contents: RunDumpContents::Text(contents),
        }
    }

    fn json(path: impl Into<String>, contents: serde_json::Value) -> Self {
        Self {
            path:     path.into(),
            contents: RunDumpContents::Json(contents),
        }
    }

    fn json_path(path: &Path, contents: serde_json::Value) -> Self {
        Self {
            path:     path_to_string(path),
            contents: RunDumpContents::Json(contents),
        }
    }

    fn bytes(path: impl Into<String>, contents: Vec<u8>) -> Self {
        Self {
            path:     path.into(),
            contents: RunDumpContents::Bytes(contents),
        }
    }

    fn bytes_path(path: &Path, contents: Vec<u8>) -> Self {
        Self {
            path:     path_to_string(path),
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
    validate_single_path_segment("node id", stage_id.node_id())?;
    let filename_path = validate_relative_path("artifact filename", filename)?;
    Ok(PathBuf::from("artifacts")
        .join(stage_id.to_string())
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};
    use fabro_store::{NodeState, RunProjection, StageId};
    use fabro_types::graph::Graph;
    use fabro_types::run::RunSpec;
    use fabro_types::{
        Checkpoint, Conclusion, NodeStatusRecord, RunStatus, SandboxRecord, StageOutcome,
        StartRecord, SuccessReason, WorkflowSettings, fixtures,
    };

    use super::RunDump;
    use crate::run_dump::RunDumpContents;

    fn sample_run_spec() -> RunSpec {
        RunSpec {
            run_id:           fixtures::RUN_1,
            settings:         WorkflowSettings::default(),
            graph:            Graph::new("ship"),
            workflow_slug:    Some("demo".to_string()),
            source_directory: Some("/tmp/project".to_string()),
            git:              Some(fabro_types::GitContext {
                origin_url:   "https://github.com/fabro-sh/fabro.git".to_string(),
                branch:       "main".to_string(),
                sha:          None,
                dirty:        fabro_types::DirtyStatus::Clean,
                push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
            }),
            labels:           HashMap::from([("team".to_string(), "platform".to_string())]),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        }
    }

    fn sample_checkpoint() -> Checkpoint {
        Checkpoint {
            timestamp:                  Utc
                .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
                .single()
                .unwrap(),
            current_node:               "build".to_string(),
            completed_nodes:            vec!["build".to_string()],
            node_retries:               HashMap::new(),
            context_values:             HashMap::new(),
            node_outcomes:              HashMap::new(),
            next_node_id:               Some("ship".to_string()),
            git_commit_sha:             Some("abc123".to_string()),
            loop_failure_signatures:    HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits:                HashMap::from([("build".to_string(), 2usize)]),
        }
    }

    #[test]
    fn from_projection_uses_stages_layout_and_collapses_top_level_metadata_files() {
        let stage_id = StageId::new("build", 2);
        let mut projection = RunProjection::default();
        projection.spec = Some(sample_run_spec());
        projection.graph_source = Some("digraph Ship {}".to_string());
        projection.start = Some(StartRecord {
            run_id:     fixtures::RUN_1,
            start_time: Utc
                .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
                .single()
                .unwrap(),
            run_branch: Some("fabro/run/demo".to_string()),
            base_sha:   Some("deadbeef".to_string()),
        });
        projection.status = Some(RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        });
        projection.checkpoint = Some(sample_checkpoint());
        projection.conclusion = Some(Conclusion {
            timestamp:            Utc
                .with_ymd_and_hms(2026, 4, 20, 12, 5, 0)
                .single()
                .unwrap(),
            status:               StageOutcome::Succeeded,
            duration_ms:          5,
            failure_reason:       None,
            final_git_commit_sha: Some("abc123".to_string()),
            stages:               Vec::new(),
            billing:              None,
            total_retries:        0,
        });
        projection.sandbox = Some(SandboxRecord {
            provider:          "local".to_string(),
            working_directory: "/tmp/project".to_string(),
            identifier:        Some("sandbox-1".to_string()),
            repo_cloned:       None,
            clone_origin_url:  None,
            clone_branch:      None,
        });
        projection.retro_prompt = Some("retro prompt".to_string());
        projection.retro_response = Some("retro response".to_string());
        projection.set_node(stage_id.clone(), NodeState {
            prompt:            Some("plan".to_string()),
            response:          Some("done".to_string()),
            status:            Some(NodeStatusRecord {
                status:         StageOutcome::Succeeded,
                notes:          Some("ok".to_string()),
                failure_reason: None,
                timestamp:      Utc
                    .with_ymd_and_hms(2026, 4, 20, 12, 1, 0)
                    .single()
                    .unwrap(),
            }),
            provider_used:     Some(serde_json::json!({ "provider": "openai" })),
            diff:              Some("diff --git a/a b/a".to_string()),
            script_invocation: Some(serde_json::json!({ "command": "cargo test" })),
            script_timing:     Some(serde_json::json!({ "duration_ms": 10 })),
            parallel_results:  Some(serde_json::json!([{ "stage": "fanout@1" }])),
            stdout:            Some("stdout".to_string()),
            stderr:            Some("stderr".to_string()),
            stdout_bytes:      None,
            stderr_bytes:      None,
            streams_separated: None,
            live_streaming:    None,
        });

        let dump = RunDump::from_projection(&projection);
        let paths: Vec<&str> = dump
            .entries()
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();

        assert!(paths.contains(&"run.json"));
        assert!(paths.contains(&"graph.fabro"));
        assert!(paths.contains(&"stages/retro/prompt.md"));
        assert!(paths.contains(&"stages/retro/response.md"));
        assert!(paths.contains(&"stages/build@2/prompt.md"));
        assert!(paths.contains(&"stages/build@2/response.md"));
        assert!(paths.contains(&"stages/build@2/status.json"));
        assert!(paths.contains(&"stages/build@2/provider_used.json"));
        assert!(paths.contains(&"stages/build@2/diff.patch"));
        assert!(paths.contains(&"stages/build@2/script_invocation.json"));
        assert!(paths.contains(&"stages/build@2/script_timing.json"));
        assert!(paths.contains(&"stages/build@2/parallel_results.json"));
        assert!(paths.contains(&"stages/build@2/stdout.log"));
        assert!(paths.contains(&"stages/build@2/stderr.log"));
        assert!(!paths.contains(&"start.json"));
        assert!(!paths.contains(&"status.json"));
        assert!(!paths.contains(&"checkpoint.json"));
        assert!(!paths.contains(&"sandbox.json"));
        assert!(!paths.contains(&"retro.json"));
        assert!(!paths.contains(&"conclusion.json"));

        let run_json = dump
            .entries()
            .iter()
            .find(|entry| entry.path == "run.json")
            .expect("run.json should be emitted");
        let RunDumpContents::Json(value) = &run_json.contents else {
            panic!("run.json should be json");
        };
        let round_tripped: RunProjection = serde_json::from_value(value.clone()).unwrap();
        let node = round_tripped.node(&stage_id).expect("node should exist");

        assert!(round_tripped.spec.is_some());
        assert!(round_tripped.start.is_some());
        assert!(round_tripped.status.is_some());
        assert!(round_tripped.checkpoint.is_some());
        assert!(round_tripped.conclusion.is_some());
        assert!(round_tripped.sandbox.is_some());
        assert_eq!(node.prompt, None);
        assert_eq!(node.response, None);
        assert_eq!(node.diff, None);
        assert_eq!(node.stdout, None);
        assert_eq!(node.stderr, None);
        assert_eq!(
            node.provider_used,
            Some(serde_json::json!({ "provider": "openai" }))
        );
    }
}
