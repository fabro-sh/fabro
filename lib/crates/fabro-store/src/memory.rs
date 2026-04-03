use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::keys;
use crate::run_state::EventProjectionCache;
use crate::{
    CatalogRecord, EventEnvelope, EventPayload, ListRunsQuery, NodeVisitRef, Result, RunState,
    RunSummary, StoreError,
};
use fabro_types::RunId;

#[derive(Debug, Default)]
pub struct InMemoryStore {
    runs: Mutex<HashMap<RunId, InMemoryCatalog>>,
}

#[derive(Debug, Clone)]
struct InMemoryCatalog {
    record: CatalogRecord,
    run_store: Arc<InMemoryRunStore>,
}

#[derive(Debug)]
pub struct InMemoryRunStore {
    run_id: RunId,
    data: Mutex<BTreeMap<String, Vec<u8>>>,
    event_seq: AtomicU32,
    watchers: Mutex<Vec<mpsc::UnboundedSender<EventEnvelope>>>,
    projection_cache: Mutex<EventProjectionCache>,
}

impl InMemoryRunStore {
    fn new(
        run_id: &RunId,
        created_at: DateTime<Utc>,
        db_prefix: String,
        run_dir: Option<String>,
    ) -> Result<Self> {
        let record = CatalogRecord {
            run_id: *run_id,
            created_at,
            db_prefix,
            run_dir,
        };
        let mut data = BTreeMap::new();
        data.insert(keys::init().to_string(), serde_json::to_vec(&record)?);
        Ok(Self {
            run_id: *run_id,
            data: Mutex::new(data),
            event_seq: AtomicU32::new(1),
            watchers: Mutex::new(Vec::new()),
            projection_cache: Mutex::new(EventProjectionCache::default()),
        })
    }

    async fn put_json<T: Serialize>(&self, key: String, value: &T) -> Result<()> {
        self.data
            .lock()
            .await
            .insert(key, serde_json::to_vec(value)?);
        Ok(())
    }

    async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let bytes = self.data.lock().await.get(key).cloned();
        bytes
            .map(|value| serde_json::from_slice(&value))
            .transpose()
            .map_err(Into::into)
    }

    async fn put_bytes(&self, key: String, value: &[u8]) {
        self.data.lock().await.insert(key, value.to_vec());
    }

    async fn get_bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.data.lock().await.get(key).cloned()
    }

    async fn snapshot_data(&self) -> BTreeMap<String, Vec<u8>> {
        self.data.lock().await.clone()
    }

    async fn list_events_from_inner(&self, seq: u32) -> Result<Vec<EventEnvelope>> {
        let data = self.snapshot_data().await;
        let mut events = Vec::new();
        for (key, value) in &data {
            let Some(current_seq) = keys::parse_event_seq(key) else {
                continue;
            };
            if current_seq < seq {
                continue;
            }
            events.push(EventEnvelope {
                seq: current_seq,
                payload: serde_json::from_slice(value)?,
            });
        }
        events.sort_by_key(|event| event.seq);
        Ok(events)
    }

    async fn list_artifact_values_inner(&self) -> Result<Vec<String>> {
        let data = self.snapshot_data().await;
        let mut artifact_ids = Vec::new();
        for key in data.keys() {
            let Some(artifact_id) = keys::parse_artifact_value_id(key) else {
                continue;
            };
            artifact_ids.push(artifact_id);
        }
        artifact_ids.sort();
        Ok(artifact_ids)
    }

    async fn list_all_assets_inner(&self) -> Result<Vec<(String, u32, String)>> {
        let data = self.snapshot_data().await;
        let mut assets = Vec::new();
        for key in data.keys() {
            let Some(asset) = keys::parse_node_asset_key(key) else {
                continue;
            };
            assets.push(asset);
        }
        assets.sort();
        Ok(assets)
    }

    async fn projected_state(&self) -> Result<RunState> {
        let next_seq = {
            let cache = self.projection_cache.lock().await;
            cache.last_seq.saturating_add(1)
        };
        let events = self.list_events_from_inner(next_seq).await?;
        let mut cache = self.projection_cache.lock().await;
        for event in &events {
            cache.state.apply_event(event)?;
            cache.last_seq = event.seq;
        }
        Ok(cache.state.clone())
    }
}

impl InMemoryStore {
    pub async fn create_run(
        &self,
        run_id: &RunId,
        created_at: DateTime<Utc>,
        run_dir: Option<&str>,
    ) -> Result<Arc<InMemoryRunStore>> {
        let mut runs = self.runs.lock().await;
        if let Some(existing) = runs.get(run_id) {
            if existing.record.created_at == created_at {
                return Ok(Arc::clone(&existing.run_store));
            }
            return Err(StoreError::RunAlreadyExists(run_id.to_string()));
        }

        let db_prefix = format!("memory/runs/{run_id}");
        let run_store = Arc::new(InMemoryRunStore::new(
            run_id,
            created_at,
            db_prefix.clone(),
            run_dir.map(ToOwned::to_owned),
        )?);
        let catalog = InMemoryCatalog {
            record: CatalogRecord {
                run_id: *run_id,
                created_at,
                db_prefix,
                run_dir: run_dir.map(ToOwned::to_owned),
            },
            run_store: run_store.clone(),
        };
        runs.insert(*run_id, catalog);
        Ok(run_store)
    }

    pub async fn open_run(&self, run_id: &RunId) -> Result<Arc<InMemoryRunStore>> {
        let runs = self.runs.lock().await;
        runs.get(run_id)
            .map(|catalog| Arc::clone(&catalog.run_store))
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))
    }

    pub async fn open_run_reader(&self, run_id: &RunId) -> Result<Arc<InMemoryRunStore>> {
        self.open_run(run_id).await
    }

    pub async fn list_runs(&self, query: &ListRunsQuery) -> Result<Vec<RunSummary>> {
        let catalogs = {
            let runs = self.runs.lock().await;
            runs.values().cloned().collect::<Vec<_>>()
        };

        let mut summaries = Vec::new();
        for catalog in catalogs {
            if !matches_query(&catalog.record.created_at, query) {
                continue;
            }
            summaries.push(
                catalog
                    .run_store
                    .state()
                    .await?
                    .build_summary(&catalog.record),
            );
        }
        summaries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(summaries)
    }

    pub async fn delete_run(&self, run_id: &RunId) -> Result<()> {
        self.runs.lock().await.remove(run_id);
        Ok(())
    }
}

impl InMemoryRunStore {
    pub async fn append_event(&self, payload: &EventPayload) -> Result<u32> {
        payload.validate(&self.run_id)?;

        let seq = self.event_seq.fetch_add(1, Ordering::SeqCst);
        let envelope = EventEnvelope {
            seq,
            payload: payload.clone(),
        };
        self.put_json(
            keys::event_key(seq, Utc::now().timestamp_millis()),
            &envelope.payload,
        )
        .await?;

        let mut watchers = self.watchers.lock().await;
        watchers.retain(|sender| sender.send(envelope.clone()).is_ok());
        Ok(seq)
    }

    pub async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        self.list_events_from_inner(1).await
    }

    pub async fn list_events_from(&self, seq: u32) -> Result<Vec<EventEnvelope>> {
        self.list_events_from_inner(seq).await
    }

    pub async fn watch_events_from(
        &self,
        seq: u32,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = Result<EventEnvelope>> + Send>>> {
        let (sender, receiver) = mpsc::unbounded_channel();

        let data = self.data.lock().await;
        let mut existing = Vec::new();
        for (key, value) in &*data {
            let Some(current_seq) = keys::parse_event_seq(key) else {
                continue;
            };
            if current_seq < seq {
                continue;
            }
            existing.push(EventEnvelope {
                seq: current_seq,
                payload: serde_json::from_slice(value)?,
            });
        }
        existing.sort_by_key(|event| event.seq);
        for envelope in existing {
            let _ = sender.send(envelope);
        }
        self.watchers.lock().await.push(sender);
        drop(data);

        Ok(Box::pin(UnboundedReceiverStream::new(receiver).map(Ok)))
    }

    pub async fn put_artifact_value(
        &self,
        artifact_id: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        self.put_json(keys::artifact_value(artifact_id), value)
            .await
    }

    pub async fn get_artifact_value(&self, artifact_id: &str) -> Result<Option<serde_json::Value>> {
        self.get_json(&keys::artifact_value(artifact_id)).await
    }

    pub async fn list_artifact_values(&self) -> Result<Vec<String>> {
        self.list_artifact_values_inner().await
    }

    pub async fn put_asset(
        &self,
        node: &NodeVisitRef<'_>,
        filename: &str,
        data: &[u8],
    ) -> Result<()> {
        self.put_bytes(keys::node_asset(node, filename), data).await;
        Ok(())
    }

    pub async fn get_asset(
        &self,
        node: &NodeVisitRef<'_>,
        filename: &str,
    ) -> Result<Option<Bytes>> {
        Ok(self
            .get_bytes(&keys::node_asset(node, filename))
            .await
            .map(Bytes::from))
    }

    pub async fn list_assets(&self, node: &NodeVisitRef<'_>) -> Result<Vec<String>> {
        let prefix = format!("{}/", keys::node_asset_prefix(node));
        let data = self.snapshot_data().await;
        let mut assets = Vec::new();
        for key in data.keys() {
            if let Some(path) = key.strip_prefix(&prefix) {
                assets.push(path.to_string());
            }
        }
        assets.sort();
        Ok(assets)
    }

    pub async fn list_all_assets(&self) -> Result<Vec<(String, u32, String)>> {
        self.list_all_assets_inner().await
    }

    pub async fn state(&self) -> Result<RunState> {
        self.projected_state().await
    }
}

fn matches_query(created_at: &DateTime<Utc>, query: &ListRunsQuery) -> bool {
    if let Some(start) = query.start {
        if *created_at < start {
            return false;
        }
    }
    if let Some(end) = query.end {
        if *created_at > end {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::Duration as ChronoDuration;
    use fabro_types::{
        AttrValue, Checkpoint, Conclusion, Graph, PullRequestRecord, Retro, RunId, RunRecord,
        RunStatus, RunStatusRecord, SandboxRecord, Settings, StageStatus, StartRecord,
        StatusReason, fixtures,
    };
    use tokio::time::timeout;

    fn dt(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn test_run_id(label: &str) -> RunId {
        match label {
            "run-1" => fixtures::RUN_1,
            "run-early" => fixtures::RUN_2,
            "run-late" => fixtures::RUN_3,
            "other-run" => fixtures::RUN_4,
            _ => panic!("unknown test run id: {label}"),
        }
    }

    fn sample_run_record(run_id: &str, created_at: DateTime<Utc>) -> RunRecord {
        let mut graph = Graph::new("night-sky");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("map the constellations".to_string()),
        );
        RunRecord {
            run_id: test_run_id(run_id),
            created_at,
            settings: Settings::default(),
            graph,
            workflow_slug: Some("night-sky".to_string()),
            working_directory: PathBuf::from("/tmp/night-sky"),
            host_repo_path: Some("github.com/fabro-sh/fabro".to_string()),
            base_branch: Some("main".to_string()),
            labels: HashMap::from([("team".to_string(), "infra".to_string())]),
        }
    }

    fn sample_start_record(run_id: &str, created_at: DateTime<Utc>) -> StartRecord {
        StartRecord {
            run_id: test_run_id(run_id),
            start_time: created_at + ChronoDuration::seconds(5),
            run_branch: Some("fabro/run/demo".to_string()),
            base_sha: Some("abc123".to_string()),
        }
    }

    fn sample_status(status: RunStatus, reason: Option<StatusReason>) -> RunStatusRecord {
        RunStatusRecord {
            status,
            reason,
            updated_at: dt("2026-03-27T12:05:00Z"),
        }
    }

    fn sample_checkpoint() -> Checkpoint {
        Checkpoint {
            timestamp: dt("2026-03-27T12:10:00Z"),
            current_node: "code".to_string(),
            completed_nodes: vec!["plan".to_string()],
            node_retries: HashMap::from([("code".to_string(), 1)]),
            context_values: HashMap::from([(
                "artifact".to_string(),
                serde_json::json!({"kind": "summary"}),
            )]),
            node_outcomes: HashMap::new(),
            next_node_id: Some("review".to_string()),
            git_commit_sha: Some("def456".to_string()),
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits: HashMap::from([("code".to_string(), 2)]),
        }
    }

    fn sample_conclusion() -> Conclusion {
        Conclusion {
            timestamp: dt("2026-03-27T12:15:00Z"),
            status: StageStatus::Success,
            duration_ms: 3210,
            failure_reason: None,
            final_git_commit_sha: Some("feedbeef".to_string()),
            stages: Vec::new(),
            total_cost: Some(1.25),
            total_retries: 2,
            total_input_tokens: 10,
            total_output_tokens: 20,
            total_cache_read_tokens: 30,
            total_cache_write_tokens: 40,
            total_reasoning_tokens: 50,
            has_pricing: true,
        }
    }

    fn sample_retro(run_id: &str) -> Retro {
        Retro {
            run_id: test_run_id(run_id),
            workflow_name: "night-sky".to_string(),
            goal: "map the constellations".to_string(),
            timestamp: dt("2026-03-27T12:20:00Z"),
            smoothness: None,
            stages: Vec::new(),
            stats: fabro_types::AggregateStats {
                total_duration_ms: 3210,
                total_cost: Some(1.25),
                total_retries: 2,
                files_touched: vec!["src/lib.rs".to_string()],
                stages_completed: 3,
                stages_failed: 0,
            },
            intent: Some("ship the fix".to_string()),
            outcome: Some("done".to_string()),
            learnings: None,
            friction_points: None,
            open_items: None,
        }
    }

    fn sample_sandbox() -> SandboxRecord {
        SandboxRecord {
            provider: "local".to_string(),
            working_directory: "/tmp/night-sky".to_string(),
            identifier: Some("sandbox-1".to_string()),
            host_working_directory: Some("/tmp/night-sky".to_string()),
            container_mount_point: None,
        }
    }

    fn sample_pull_request() -> PullRequestRecord {
        PullRequestRecord {
            html_url: "https://github.com/fabro-sh/fabro/pull/123".to_string(),
            number: 123,
            owner: "fabro-sh".to_string(),
            repo: "fabro".to_string(),
            base_branch: "main".to_string(),
            head_branch: "fabro/run/demo".to_string(),
            title: "Map the constellations".to_string(),
        }
    }

    fn event_payload(
        run_id: &str,
        ts: &str,
        event: &str,
        node_id: Option<&str>,
        properties: serde_json::Value,
    ) -> EventPayload {
        let mut value = serde_json::json!({
            "id": format!("evt-{event}-{ts}"),
            "ts": ts,
            "run_id": test_run_id(run_id).to_string(),
            "event": event,
            "properties": properties,
        });
        if let Some(node_id) = node_id {
            value["node_id"] = serde_json::Value::String(node_id.to_string());
        }
        EventPayload::new(value, &test_run_id(run_id)).unwrap()
    }

    #[tokio::test]
    async fn create_run_state_and_node_storage_round_trip() {
        let store = InMemoryStore::default();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();

        let run_record = sample_run_record("run-1", created_at);
        let start_record = sample_start_record("run-1", created_at);
        let status_record =
            sample_status(RunStatus::Running, Some(StatusReason::SandboxInitializing));
        let checkpoint = sample_checkpoint();
        let conclusion = sample_conclusion();
        let retro = sample_retro("run-1");
        let sandbox = sample_sandbox();
        let node = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        let pull_request = sample_pull_request();

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": run_record.settings,
                "graph": run_record.graph,
                "workflow_source": "digraph night_sky {}",
                "workflow_slug": run_record.workflow_slug,
                "working_directory": run_record.working_directory,
                "host_repo_path": run_record.host_repo_path,
                "base_branch": run_record.base_branch,
                "labels": run_record.labels,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:05Z",
            "run.started",
            None,
            serde_json::json!({
                "run_branch": start_record.run_branch,
                "base_sha": start_record.base_sha,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:06Z",
            "run.running",
            None,
            serde_json::json!({
                "reason": status_record.reason,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:07Z",
            "checkpoint.completed",
            Some("code"),
            serde_json::json!({
                "status": "success",
                "current_node": checkpoint.current_node,
                "completed_nodes": checkpoint.completed_nodes,
                "node_retries": checkpoint.node_retries,
                "context_values": checkpoint.context_values,
                "node_outcomes": checkpoint.node_outcomes,
                "next_node_id": checkpoint.next_node_id,
                "git_commit_sha": checkpoint.git_commit_sha,
                "loop_failure_signatures": serde_json::json!({}),
                "restart_failure_signatures": serde_json::json!({}),
                "node_visits": checkpoint.node_visits,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08Z",
            "sandbox.initialized",
            None,
            serde_json::json!({
                "provider": sandbox.provider,
                "working_directory": sandbox.working_directory,
                "identifier": sandbox.identifier,
                "host_working_directory": sandbox.host_working_directory,
                "container_mount_point": sandbox.container_mount_point,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.1Z",
            "stage.prompt",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "text": "Plan the fix",
                "mode": "prompt",
                "provider": "openai",
                "model": "gpt-5.4"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.2Z",
            "command.started",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "command": "cargo test"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.3Z",
            "command.completed",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "stdout": "ok",
                "stderr": "",
                "exit_code": 0
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.4Z",
            "checkpoint.completed",
            Some("code"),
            serde_json::json!({
                "status": "success",
                "ordinal": 2,
                "current_node": checkpoint.current_node,
                "completed_nodes": checkpoint.completed_nodes,
                "node_retries": checkpoint.node_retries,
                "context_values": checkpoint.context_values,
                "node_outcomes": checkpoint.node_outcomes,
                "next_node_id": checkpoint.next_node_id,
                "git_commit_sha": checkpoint.git_commit_sha,
                "node_visits": checkpoint.node_visits,
                "diff": "diff --git a/src/lib.rs b/src/lib.rs"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.5Z",
            "parallel.completed",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "results": [{"node_id": "lint", "status": "success"}]
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.6Z",
            "stage.completed",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "status": "success",
                "notes": "all good",
                "response": "Implemented",
                "files_touched": ["src/lib.rs"]
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.7Z",
            "retro.started",
            None,
            serde_json::json!({
                "prompt": "How did it go?"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:09Z",
            "retro.completed",
            None,
            serde_json::json!({
                "response": "Smooth enough",
                "retro": retro,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:10Z",
            "run.completed",
            None,
            serde_json::json!({
                "status": conclusion.status,
                "duration_ms": conclusion.duration_ms,
                "total_cost": conclusion.total_cost,
                "final_git_commit_sha": conclusion.final_git_commit_sha,
                "final_patch": "diff --git a/src/lib.rs b/src/lib.rs\n",
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:11Z",
            "pull_request.created",
            None,
            serde_json::json!({
                "pr_url": pull_request.html_url,
                "pr_number": pull_request.number,
                "owner": pull_request.owner,
                "repo": pull_request.repo,
                "base_branch": pull_request.base_branch,
                "head_branch": pull_request.head_branch,
                "title": pull_request.title,
            }),
        ))
        .await
        .unwrap();
        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
            .await
            .unwrap();
        run.put_asset(&node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let state = run.state().await.unwrap();
        let stored_run = state.run.as_ref().unwrap();
        assert_eq!(stored_run.run_id, run_record.run_id);
        assert_eq!(stored_run.created_at, run_record.created_at);
        assert_eq!(stored_run.workflow_slug, run_record.workflow_slug);
        assert_eq!(stored_run.graph.name, run_record.graph.name);
        assert_eq!(state.graph_source.as_deref(), Some("digraph night_sky {}"));

        let stored_start = state.start.as_ref().unwrap();
        assert_eq!(stored_start.run_id, start_record.run_id);
        assert_eq!(stored_start.start_time, start_record.start_time);

        let stored_status = state.status.as_ref().unwrap();
        assert_eq!(stored_status.status, RunStatus::Succeeded);
        assert_eq!(stored_status.reason, None);

        let stored_checkpoint = state.checkpoint.as_ref().unwrap();
        assert_eq!(stored_checkpoint.current_node, checkpoint.current_node);
        assert_eq!(stored_checkpoint.next_node_id, checkpoint.next_node_id);

        let stored_conclusion = state.conclusion.as_ref().unwrap();
        assert_eq!(stored_conclusion.status, conclusion.status);
        assert_eq!(stored_conclusion.duration_ms, conclusion.duration_ms);
        assert_eq!(stored_conclusion.total_cost, conclusion.total_cost);

        let stored_retro = state.retro.as_ref().unwrap();
        assert_eq!(stored_retro.run_id, retro.run_id);
        assert_eq!(stored_retro.intent, retro.intent);
        let stored_sandbox = state.sandbox.as_ref().unwrap();
        assert_eq!(stored_sandbox.provider, sandbox.provider);
        assert_eq!(stored_sandbox.working_directory, sandbox.working_directory);
        assert_eq!(state.retro_prompt.as_deref(), Some("How did it go?"));
        assert_eq!(state.retro_response.as_deref(), Some("Smooth enough"));
        assert_eq!(
            run.get_artifact_value("summary").await.unwrap(),
            Some(serde_json::json!({"done": true}))
        );
        assert_eq!(
            run.get_asset(&node, "src/lib.rs").await.unwrap(),
            Some(Bytes::from_static(b"fn main() {}"))
        );
        assert_eq!(
            state.final_patch.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs\n")
        );
        assert_eq!(state.pull_request, Some(pull_request.clone()));
        assert_eq!(state.list_node_ids(), vec!["code".to_string()]);
        let node_state = state
            .node(&node)
            .expect("node state should exist for code:2");
        assert_eq!(node_state.prompt.as_deref(), Some("Plan the fix"));
        assert_eq!(node_state.response.as_deref(), Some("Implemented"));
        assert_eq!(node_state.stdout.as_deref(), Some("ok"));
        assert_eq!(node_state.stderr.as_deref(), Some(""));
        assert_eq!(
            node_state.diff.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs")
        );
        assert_eq!(
            node_state
                .provider_used
                .as_ref()
                .and_then(|v| v.get("provider"))
                .and_then(|v| v.as_str()),
            Some("openai")
        );
        assert_eq!(
            run.list_assets(&node).await.unwrap(),
            vec!["src/lib.rs".to_string()]
        );
    }

    #[tokio::test]
    async fn state_projects_event_stream() {
        let store = InMemoryStore::default();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        let run_record = sample_run_record("run-1", created_at);
        let retro = sample_retro("run-1");

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": run_record.settings,
                "graph": run_record.graph,
                "workflow_source": "digraph night_sky {}",
                "workflow_slug": run_record.workflow_slug,
                "working_directory": run_record.working_directory,
                "host_repo_path": run_record.host_repo_path,
                "base_branch": run_record.base_branch,
                "labels": run_record.labels,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:05Z",
            "run.started",
            None,
            serde_json::json!({
                "run_branch": "fabro/run/demo",
                "base_sha": "abc123"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:06Z",
            "run.running",
            None,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:07Z",
            "stage.prompt",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "text": "Plan the fix",
                "mode": "prompt",
                "provider": "openai",
                "model": "gpt-5.4"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08Z",
            "stage.completed",
            Some("code"),
            serde_json::json!({
                "status": "success",
                "notes": "all good",
                "response": "Implemented",
                "files_touched": ["src/lib.rs"],
                "node_visits": {"code": 2}
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:09Z",
            "checkpoint.completed",
            Some("code"),
            serde_json::json!({
                "status": "success",
                "current_node": "code",
                "completed_nodes": ["plan"],
                "context_values": {"artifact": {"kind": "summary"}},
                "next_node_id": "review",
                "git_commit_sha": "def456",
                "node_visits": {"code": 2},
                "diff": "diff --git a/src/lib.rs b/src/lib.rs"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:10Z",
            "sandbox.initialized",
            None,
            serde_json::json!({
                "provider": "local",
                "working_directory": "/tmp/night-sky",
                "identifier": "sandbox-1",
                "host_working_directory": "/tmp/night-sky"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:11Z",
            "retro.started",
            None,
            serde_json::json!({
                "prompt": "How did it go?"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:12Z",
            "retro.completed",
            None,
            serde_json::json!({
                "response": "Smooth enough",
                "retro": retro
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:13Z",
            "pull_request.created",
            None,
            serde_json::json!({
                "pr_url": "https://github.com/fabro-sh/fabro/pull/123",
                "pr_number": 123,
                "owner": "fabro-sh",
                "repo": "fabro",
                "base_branch": "main",
                "head_branch": "fabro/run/demo",
                "title": "Map the constellations",
                "draft": false
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:15Z",
            "run.completed",
            None,
            serde_json::json!({
                "duration_ms": 3210,
                "artifact_count": 1,
                "status": "success",
                "total_cost": 1.25,
                "final_git_commit_sha": "feedbeef",
                "final_patch": "diff --git a/src/lib.rs b/src/lib.rs\n"
            }),
        ))
        .await
        .unwrap();

        let state = run.state().await.unwrap();
        assert_eq!(
            state.run.as_ref().map(|run| run.run_id),
            Some(test_run_id("run-1"))
        );
        assert_eq!(state.graph_source.as_deref(), Some("digraph night_sky {}"));
        assert_eq!(
            state
                .start
                .as_ref()
                .and_then(|start| start.run_branch.as_deref()),
            Some("fabro/run/demo")
        );
        assert_eq!(
            state.status.as_ref().map(|status| status.status),
            Some(RunStatus::Succeeded)
        );
        assert_eq!(
            state
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.current_node.as_str()),
            Some("code")
        );
        assert_eq!(state.checkpoints.len(), 1);
        assert_eq!(
            state.final_patch.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs\n")
        );
        assert_eq!(state.retro_prompt.as_deref(), Some("How did it go?"));
        assert_eq!(state.retro_response.as_deref(), Some("Smooth enough"));
        assert_eq!(state.pull_request.as_ref().map(|pr| pr.number), Some(123));
        assert_eq!(
            state
                .sandbox
                .as_ref()
                .map(|sandbox| sandbox.provider.as_str()),
            Some("local")
        );
        assert_eq!(state.list_node_visits("code"), vec![2]);
        let node = state
            .node(&NodeVisitRef {
                node_id: "code",
                visit: 2,
            })
            .unwrap();
        assert_eq!(node.prompt.as_deref(), Some("Plan the fix"));
        assert_eq!(node.response.as_deref(), Some("Implemented"));
        assert_eq!(
            node.diff.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs")
        );
        assert_eq!(
            node.provider_used
                .as_ref()
                .and_then(|value| value.get("provider"))
                .and_then(|value| value.as_str()),
            Some("openai")
        );
    }

    #[tokio::test]
    async fn state_rewind_keeps_active_projection_only() {
        let store = InMemoryStore::default();
        let run = store
            .create_run(&test_run_id("run-1"), dt("2026-03-27T12:00:00Z"), None)
            .await
            .unwrap();

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": Settings::default(),
                "graph": Graph::new("night-sky"),
                "working_directory": "/tmp/night-sky",
                "labels": {}
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:01Z",
            "stage.prompt",
            Some("code"),
            serde_json::json!({
                "visit": 1,
                "text": "before rewind"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "pull_request.created",
            None,
            serde_json::json!({
                "pr_url": "https://github.com/fabro-sh/fabro/pull/123",
                "pr_number": 123,
                "owner": "fabro-sh",
                "repo": "fabro",
                "base_branch": "main",
                "head_branch": "fabro/run/demo",
                "title": "Map the constellations",
                "draft": false
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:03Z",
            "run.completed",
            None,
            serde_json::json!({
                "duration_ms": 10,
                "artifact_count": 0,
                "status": "success",
                "final_patch": "old patch"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:04Z",
            "run.rewound",
            None,
            serde_json::json!({
                "target_checkpoint_ordinal": 1,
                "target_node_id": "plan",
                "target_visit": 1
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:05Z",
            "checkpoint.completed",
            Some("plan"),
            serde_json::json!({
                "status": "success",
                "current_node": "plan",
                "completed_nodes": [],
                "node_visits": {"plan": 1}
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:06Z",
            "run.submitted",
            None,
            serde_json::json!({}),
        ))
        .await
        .unwrap();

        let state = run.state().await.unwrap();
        assert_eq!(
            state.status.as_ref().map(|status| status.status),
            Some(RunStatus::Submitted)
        );
        assert!(state.conclusion.is_none());
        assert!(state.final_patch.is_none());
        assert!(state.pull_request.is_none());
        assert_eq!(state.checkpoints.len(), 1);
        assert_eq!(
            state
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.current_node.as_str()),
            Some("plan")
        );
        assert!(state.list_node_ids().is_empty());
    }

    #[tokio::test]
    async fn list_artifact_values_and_all_assets_include_asset_only_visits() {
        let store = InMemoryStore::default();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();

        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
            .await
            .unwrap();
        run.put_artifact_value("plan", &serde_json::json!({"steps": 3}))
            .await
            .unwrap();

        let snapshot_node = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        run.put_asset(&snapshot_node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let asset_only_node = NodeVisitRef {
            node_id: "artifact-only",
            visit: 7,
        };
        run.put_asset(&asset_only_node, "logs/output.txt", b"hello")
            .await
            .unwrap();

        assert_eq!(
            run.list_artifact_values().await.unwrap(),
            vec!["plan".to_string(), "summary".to_string()]
        );
        assert_eq!(
            run.list_all_assets().await.unwrap(),
            vec![
                (
                    "artifact-only".to_string(),
                    7,
                    "logs/output.txt".to_string()
                ),
                ("code".to_string(), 2, "src/lib.rs".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn append_event_validates_payload_shape_and_run_id() {
        let store = InMemoryStore::default();
        let run = store
            .create_run(&test_run_id("run-1"), dt("2026-03-27T12:00:00Z"), None)
            .await
            .unwrap();

        let invalid_missing: EventPayload = serde_json::from_value(serde_json::json!({
            "run_id": "run-1"
        }))
        .unwrap();
        let err = run.append_event(&invalid_missing).await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidEvent(_)));

        let invalid_run_id: EventPayload = serde_json::from_value(serde_json::json!({
            "id": "evt-invalid-run",
            "ts": "2026-03-27T12:00:00Z",
            "run_id": "other-run",
            "event": "StageStarted"
        }))
        .unwrap();
        let err = run.append_event(&invalid_run_id).await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidEvent(_)));
    }

    #[tokio::test]
    async fn watch_events_from_receives_existing_and_live_events() {
        let store = InMemoryStore::default();
        let run = store
            .create_run(&test_run_id("run-1"), dt("2026-03-27T12:00:00Z"), None)
            .await
            .unwrap();
        let first = EventPayload::new(
            serde_json::json!({
                "id": "evt-1",
                "ts": "2026-03-27T12:00:00.000Z",
                "run_id": test_run_id("run-1").to_string(),
                "event": "WorkflowRunStarted"
            }),
            &test_run_id("run-1"),
        )
        .unwrap();
        let second = EventPayload::new(
            serde_json::json!({
                "id": "evt-2",
                "ts": "2026-03-27T12:00:01.000Z",
                "run_id": test_run_id("run-1").to_string(),
                "event": "StageCompleted"
            }),
            &test_run_id("run-1"),
        )
        .unwrap();

        run.append_event(&first).await.unwrap();

        let mut stream = run.watch_events_from(1).await.unwrap();
        let existing = timeout(
            Duration::from_secs(1),
            futures::StreamExt::next(&mut stream),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(existing.seq, 1);

        run.append_event(&second).await.unwrap();
        let live = timeout(
            Duration::from_secs(1),
            futures::StreamExt::next(&mut stream),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(live.seq, 2);
    }

    #[tokio::test]
    async fn state_retains_checkpoint_history_by_event_sequence() {
        let store = InMemoryStore::default();
        let run = store
            .create_run(&test_run_id("run-1"), dt("2026-03-27T12:00:00Z"), None)
            .await
            .unwrap();
        let checkpoint = sample_checkpoint();
        let seq = run
            .append_event(&event_payload(
                "run-1",
                "2026-03-27T12:00:00Z",
                "checkpoint.completed",
                Some(&checkpoint.current_node),
                serde_json::json!({
                    "status": "success",
                    "current_node": checkpoint.current_node,
                    "completed_nodes": checkpoint.completed_nodes,
                    "node_retries": checkpoint.node_retries,
                    "context_values": checkpoint.context_values,
                    "node_outcomes": checkpoint.node_outcomes,
                    "next_node_id": checkpoint.next_node_id,
                    "git_commit_sha": checkpoint.git_commit_sha,
                    "loop_failure_signatures": serde_json::json!({}),
                    "restart_failure_signatures": serde_json::json!({}),
                    "node_visits": checkpoint.node_visits,
                }),
            ))
            .await
            .unwrap();
        let state = run.state().await.unwrap();
        assert_eq!(seq, 1);
        assert_eq!(state.checkpoints.len(), 1);
        assert_eq!(state.checkpoints[0].0, 1);
        assert_eq!(state.checkpoints[0].1.current_node, checkpoint.current_node);
        assert_eq!(
            state.checkpoint.as_ref().unwrap().current_node,
            checkpoint.current_node
        );
    }

    #[tokio::test]
    async fn list_runs_filters_dates_and_tolerates_missing_status() {
        let store = InMemoryStore::default();
        let early = dt("2026-03-27T10:00:00Z");
        let late = dt("2026-03-27T12:00:00Z");

        let early_run = store
            .create_run(&test_run_id("run-early"), early, None)
            .await
            .unwrap();
        let early_record = sample_run_record("run-early", early);
        early_run
            .append_event(&event_payload(
                "run-early",
                "2026-03-27T10:00:00Z",
                "run.created",
                None,
                serde_json::json!({
                    "settings": early_record.settings,
                    "graph": early_record.graph,
                    "workflow_slug": early_record.workflow_slug,
                    "working_directory": early_record.working_directory,
                    "host_repo_path": early_record.host_repo_path,
                    "base_branch": early_record.base_branch,
                    "labels": early_record.labels,
                }),
            ))
            .await
            .unwrap();

        let late_run = store
            .create_run(&test_run_id("run-late"), late, None)
            .await
            .unwrap();
        let late_record = sample_run_record("run-late", late);
        late_run
            .append_event(&event_payload(
                "run-late",
                "2026-03-27T12:00:00Z",
                "run.created",
                None,
                serde_json::json!({
                    "settings": late_record.settings,
                    "graph": late_record.graph,
                    "workflow_slug": late_record.workflow_slug,
                    "working_directory": late_record.working_directory,
                    "host_repo_path": late_record.host_repo_path,
                    "base_branch": late_record.base_branch,
                    "labels": late_record.labels,
                }),
            ))
            .await
            .unwrap();
        late_run
            .append_event(&event_payload(
                "run-late",
                "2026-03-27T12:00:01Z",
                "run.started",
                None,
                serde_json::json!({
                    "run_branch": "fabro/run/demo",
                    "base_sha": "abc123",
                }),
            ))
            .await
            .unwrap();
        late_run
            .append_event(&event_payload(
                "run-late",
                "2026-03-27T12:00:02Z",
                "run.completed",
                None,
                serde_json::json!({
                    "duration_ms": 3210,
                    "artifact_count": 1,
                    "status": "success",
                    "reason": "completed",
                    "total_cost": 1.25,
                }),
            ))
            .await
            .unwrap();

        let all = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].run_id, test_run_id("run-late"));
        assert_eq!(all[0].workflow_name, Some("night-sky".to_string()));
        assert_eq!(all[0].goal, Some("map the constellations".to_string()));
        assert_eq!(
            all[0].host_repo_path,
            Some("github.com/fabro-sh/fabro".to_string())
        );
        assert_eq!(all[0].duration_ms, Some(3210));
        assert_eq!(all[0].total_cost, Some(1.25));
        assert_eq!(all[0].status_reason, Some(StatusReason::Completed));
        assert_eq!(all[1].status, None);

        let filtered = store
            .list_runs(&ListRunsQuery {
                start: Some(dt("2026-03-27T11:00:00Z")),
                end: Some(dt("2026-03-27T13:00:00Z")),
            })
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, test_run_id("run-late"));
    }

    #[tokio::test]
    async fn delete_run_is_idempotent() {
        let store = InMemoryStore::default();
        store
            .create_run(&test_run_id("run-1"), dt("2026-03-27T12:00:00Z"), None)
            .await
            .unwrap();
        store.delete_run(&test_run_id("run-1")).await.unwrap();
        store.delete_run(&test_run_id("run-1")).await.unwrap();
        assert!(matches!(
            store.open_run(&test_run_id("run-1")).await,
            Err(StoreError::RunNotFound(_))
        ));
    }

    #[tokio::test]
    async fn create_run_allows_retry_and_rejects_conflict() {
        let store = InMemoryStore::default();
        let ts = dt("2026-03-27T12:00:00Z");

        // First create succeeds.
        store
            .create_run(&test_run_id("run-1"), ts, None)
            .await
            .unwrap();

        // Retry with exact same created_at succeeds (idempotent).
        store
            .create_run(&test_run_id("run-1"), ts, None)
            .await
            .unwrap();

        // Different created_at for the same run_id is rejected.
        let different_ts = dt("2026-03-27T12:00:01Z");
        match store
            .create_run(&test_run_id("run-1"), different_ts, None)
            .await
        {
            Err(StoreError::RunAlreadyExists(_)) => {} // expected
            Err(other) => panic!("expected RunAlreadyExists, got: {other:?}"),
            Ok(_) => panic!("expected RunAlreadyExists, but create_run succeeded"),
        }
    }
}
