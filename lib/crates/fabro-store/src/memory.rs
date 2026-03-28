use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::keys;
use crate::{
    CatalogRecord, EventEnvelope, EventPayload, ListRunsQuery, NodeSnapshot, NodeVisitRef, Result,
    RunSnapshot, RunStore, RunSummary, Store, StoreError,
};
use fabro_types::{
    Checkpoint, Conclusion, NodeStatusRecord, Retro, RunRecord, RunStatusRecord, SandboxRecord,
    StartRecord,
};

#[derive(Debug, Default)]
pub struct InMemoryStore {
    runs: Mutex<HashMap<String, InMemoryCatalog>>,
}

#[derive(Debug, Clone)]
struct InMemoryCatalog {
    record: CatalogRecord,
    run_store: Arc<InMemoryRunStore>,
}

#[derive(Debug)]
struct InMemoryRunStore {
    run_id: String,
    created_at: DateTime<Utc>,
    data: Mutex<BTreeMap<String, Vec<u8>>>,
    event_seq: AtomicU32,
    checkpoint_seq: AtomicU32,
    watchers: Mutex<Vec<mpsc::UnboundedSender<EventEnvelope>>>,
}

impl InMemoryRunStore {
    fn new(run_id: &str, created_at: DateTime<Utc>, db_prefix: String) -> Result<Self> {
        let record = CatalogRecord {
            run_id: run_id.to_string(),
            created_at,
            db_prefix,
        };
        let mut data = BTreeMap::new();
        data.insert(keys::init().to_string(), serde_json::to_vec(&record)?);
        Ok(Self {
            run_id: run_id.to_string(),
            created_at,
            data: Mutex::new(data),
            event_seq: AtomicU32::new(1),
            checkpoint_seq: AtomicU32::new(1),
            watchers: Mutex::new(Vec::new()),
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

    async fn put_text(&self, key: String, value: &str) {
        self.data
            .lock()
            .await
            .insert(key, value.as_bytes().to_vec());
    }

    async fn get_text(&self, key: &str) -> Result<Option<String>> {
        let bytes = self.data.lock().await.get(key).cloned();
        bytes
            .map(|value| {
                String::from_utf8(value).map_err(|err| {
                    StoreError::Other(format!("stored text is not valid UTF-8: {err}"))
                })
            })
            .transpose()
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

    async fn build_node_snapshot_from_data(
        &self,
        data: &BTreeMap<String, Vec<u8>>,
        node: &NodeVisitRef<'_>,
    ) -> Result<NodeSnapshot> {
        Ok(NodeSnapshot {
            node_id: node.node_id.to_string(),
            visit: node.visit,
            prompt: read_text(data, &keys::node_prompt(node))?,
            response: read_text(data, &keys::node_response(node))?,
            status: read_json(data, &keys::node_status(node))?,
            stdout: read_text(data, &keys::node_stdout(node))?,
            stderr: read_text(data, &keys::node_stderr(node))?,
        })
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

    async fn list_checkpoints_inner(&self) -> Result<Vec<(u32, Checkpoint)>> {
        let data = self.snapshot_data().await;
        let mut checkpoints = Vec::new();
        for (key, value) in &data {
            let Some(seq) = keys::parse_checkpoint_seq(key) else {
                continue;
            };
            checkpoints.push((seq, serde_json::from_slice(value)?));
        }
        checkpoints.sort_by_key(|(seq, _)| *seq);
        Ok(checkpoints)
    }

    async fn build_snapshot_from_data(
        &self,
        data: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Option<RunSnapshot>> {
        let Some(run) = read_json::<RunRecord>(data, keys::run())? else {
            return Ok(None);
        };

        let mut visits = BTreeSet::new();
        for key in data.keys() {
            if let Some((node_id, visit, _)) = keys::parse_node_key(key) {
                visits.insert((node_id, visit));
            }
        }

        let mut nodes = Vec::new();
        for (node_id, visit) in visits {
            let node = NodeVisitRef {
                node_id: &node_id,
                visit,
            };
            nodes.push(self.build_node_snapshot_from_data(data, &node).await?);
        }

        Ok(Some(RunSnapshot {
            run,
            start: read_json(data, keys::start())?,
            status: read_json(data, keys::status())?,
            checkpoint: read_json(data, keys::checkpoint())?,
            conclusion: read_json(data, keys::conclusion())?,
            retro: read_json(data, keys::retro())?,
            graph: read_text(data, keys::graph())?,
            sandbox: read_json(data, keys::sandbox())?,
            nodes,
        }))
    }

    fn validate_run_record(&self, record: &RunRecord) -> Result<()> {
        if record.created_at != self.created_at {
            return Err(StoreError::Other(format!(
                "run record created_at {:?} does not match store created_at {:?}",
                record.created_at, self.created_at
            )));
        }
        if record.run_id != self.run_id {
            return Err(StoreError::Other(format!(
                "run record run_id {:?} does not match store run_id {:?}",
                record.run_id, self.run_id
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl Store for InMemoryStore {
    async fn create_run(
        &self,
        run_id: &str,
        created_at: DateTime<Utc>,
    ) -> Result<Box<dyn RunStore>> {
        let mut runs = self.runs.lock().await;
        if let Some(existing) = runs.get(run_id) {
            if existing.record.created_at == created_at {
                return Ok(Box::new(Arc::clone(&existing.run_store)));
            }
            return Err(StoreError::RunAlreadyExists(run_id.to_string()));
        }

        let db_prefix = format!("memory/runs/{run_id}");
        let run_store = Arc::new(InMemoryRunStore::new(
            run_id,
            created_at,
            db_prefix.clone(),
        )?);
        let catalog = InMemoryCatalog {
            record: CatalogRecord {
                run_id: run_id.to_string(),
                created_at,
                db_prefix,
            },
            run_store: Arc::clone(&run_store),
        };
        runs.insert(run_id.to_string(), catalog);
        Ok(Box::new(run_store))
    }

    async fn open_run(&self, run_id: &str) -> Result<Option<Box<dyn RunStore>>> {
        let runs = self.runs.lock().await;
        Ok(runs
            .get(run_id)
            .map(|catalog| Box::new(Arc::clone(&catalog.run_store)) as Box<dyn RunStore>))
    }

    async fn list_runs(&self, query: &ListRunsQuery) -> Result<Vec<RunSummary>> {
        let catalogs = {
            let runs = self.runs.lock().await;
            runs.values().cloned().collect::<Vec<_>>()
        };

        let mut summaries = Vec::new();
        for catalog in catalogs {
            if !matches_query(&catalog.record.created_at, query) {
                continue;
            }
            let data = catalog.run_store.snapshot_data().await;
            summaries.push(build_run_summary(&catalog.record, &data)?);
        }
        summaries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(summaries)
    }

    async fn delete_run(&self, run_id: &str) -> Result<()> {
        self.runs.lock().await.remove(run_id);
        Ok(())
    }
}

#[async_trait]
impl RunStore for Arc<InMemoryRunStore> {
    async fn put_run(&self, record: &RunRecord) -> Result<()> {
        self.validate_run_record(record)?;
        self.put_json(keys::run().to_string(), record).await
    }

    async fn get_run(&self) -> Result<Option<RunRecord>> {
        self.get_json(keys::run()).await
    }

    async fn put_start(&self, record: &StartRecord) -> Result<()> {
        self.put_json(keys::start().to_string(), record).await
    }

    async fn get_start(&self) -> Result<Option<StartRecord>> {
        self.get_json(keys::start()).await
    }

    async fn put_status(&self, record: &RunStatusRecord) -> Result<()> {
        self.put_json(keys::status().to_string(), record).await
    }

    async fn get_status(&self) -> Result<Option<RunStatusRecord>> {
        self.get_json(keys::status()).await
    }

    async fn put_checkpoint(&self, record: &Checkpoint) -> Result<()> {
        self.put_json(keys::checkpoint().to_string(), record).await
    }

    async fn get_checkpoint(&self) -> Result<Option<Checkpoint>> {
        self.get_json(keys::checkpoint()).await
    }

    async fn append_checkpoint(&self, record: &Checkpoint) -> Result<u32> {
        let seq = self.checkpoint_seq.fetch_add(1, Ordering::SeqCst);
        let now = Utc::now().timestamp_millis();
        self.put_checkpoint(record).await?;
        self.put_json(keys::checkpoint_history_key(seq, now), record)
            .await?;
        Ok(seq)
    }

    async fn list_checkpoints(&self) -> Result<Vec<(u32, Checkpoint)>> {
        self.list_checkpoints_inner().await
    }

    async fn put_conclusion(&self, record: &Conclusion) -> Result<()> {
        self.put_json(keys::conclusion().to_string(), record).await
    }

    async fn get_conclusion(&self) -> Result<Option<Conclusion>> {
        self.get_json(keys::conclusion()).await
    }

    async fn put_retro(&self, retro: &Retro) -> Result<()> {
        self.put_json(keys::retro().to_string(), retro).await
    }

    async fn get_retro(&self) -> Result<Option<Retro>> {
        self.get_json(keys::retro()).await
    }

    async fn put_graph(&self, dot_source: &str) -> Result<()> {
        self.put_text(keys::graph().to_string(), dot_source).await;
        Ok(())
    }

    async fn get_graph(&self) -> Result<Option<String>> {
        self.get_text(keys::graph()).await
    }

    async fn put_sandbox(&self, record: &SandboxRecord) -> Result<()> {
        self.put_json(keys::sandbox().to_string(), record).await
    }

    async fn get_sandbox(&self) -> Result<Option<SandboxRecord>> {
        self.get_json(keys::sandbox()).await
    }

    async fn put_node_prompt(&self, node: &NodeVisitRef<'_>, prompt: &str) -> Result<()> {
        self.put_text(keys::node_prompt(node), prompt).await;
        Ok(())
    }

    async fn put_node_response(&self, node: &NodeVisitRef<'_>, response: &str) -> Result<()> {
        self.put_text(keys::node_response(node), response).await;
        Ok(())
    }

    async fn put_node_status(
        &self,
        node: &NodeVisitRef<'_>,
        status: &NodeStatusRecord,
    ) -> Result<()> {
        self.put_json(keys::node_status(node), status).await
    }

    async fn put_node_stdout(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()> {
        self.put_text(keys::node_stdout(node), log).await;
        Ok(())
    }

    async fn put_node_stderr(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()> {
        self.put_text(keys::node_stderr(node), log).await;
        Ok(())
    }

    async fn get_node(&self, node: &NodeVisitRef<'_>) -> Result<NodeSnapshot> {
        let data = self.snapshot_data().await;
        self.build_node_snapshot_from_data(&data, node).await
    }

    async fn list_node_visits(&self, node_id: &str) -> Result<Vec<u32>> {
        let data = self.snapshot_data().await;
        let mut visits = BTreeSet::new();
        for key in data.keys() {
            let Some((current_node_id, visit, _)) = keys::parse_node_key(key) else {
                continue;
            };
            if current_node_id == node_id {
                visits.insert(visit);
            }
        }
        Ok(visits.into_iter().collect())
    }

    async fn append_event(&self, payload: &EventPayload) -> Result<u32> {
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

    async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        self.list_events_from_inner(1).await
    }

    async fn list_events_from(&self, seq: u32) -> Result<Vec<EventEnvelope>> {
        self.list_events_from_inner(seq).await
    }

    async fn watch_events_from(
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

    async fn put_retro_prompt(&self, text: &str) -> Result<()> {
        self.put_text(keys::retro_prompt().to_string(), text).await;
        Ok(())
    }

    async fn get_retro_prompt(&self) -> Result<Option<String>> {
        self.get_text(keys::retro_prompt()).await
    }

    async fn put_retro_response(&self, text: &str) -> Result<()> {
        self.put_text(keys::retro_response().to_string(), text)
            .await;
        Ok(())
    }

    async fn get_retro_response(&self) -> Result<Option<String>> {
        self.get_text(keys::retro_response()).await
    }

    async fn put_artifact_value(&self, artifact_id: &str, value: &serde_json::Value) -> Result<()> {
        self.put_json(keys::artifact_value(artifact_id), value)
            .await
    }

    async fn get_artifact_value(&self, artifact_id: &str) -> Result<Option<serde_json::Value>> {
        self.get_json(&keys::artifact_value(artifact_id)).await
    }

    async fn put_asset(&self, node: &NodeVisitRef<'_>, filename: &str, data: &[u8]) -> Result<()> {
        self.put_bytes(keys::node_asset(node, filename), data).await;
        Ok(())
    }

    async fn get_asset(&self, node: &NodeVisitRef<'_>, filename: &str) -> Result<Option<Bytes>> {
        Ok(self
            .get_bytes(&keys::node_asset(node, filename))
            .await
            .map(Bytes::from))
    }

    async fn list_assets(&self, node: &NodeVisitRef<'_>) -> Result<Vec<String>> {
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

    async fn get_snapshot(&self) -> Result<Option<RunSnapshot>> {
        let data = self.snapshot_data().await;
        self.build_snapshot_from_data(&data).await
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

fn read_json<T: DeserializeOwned>(
    data: &BTreeMap<String, Vec<u8>>,
    key: &str,
) -> Result<Option<T>> {
    data.get(key)
        .map(|value| serde_json::from_slice(value))
        .transpose()
        .map_err(Into::into)
}

fn read_text(data: &BTreeMap<String, Vec<u8>>, key: &str) -> Result<Option<String>> {
    data.get(key)
        .map(|value| {
            String::from_utf8(value.clone())
                .map_err(|err| StoreError::Other(format!("stored text is not valid UTF-8: {err}")))
        })
        .transpose()
}

fn build_run_summary(
    record: &CatalogRecord,
    data: &BTreeMap<String, Vec<u8>>,
) -> Result<RunSummary> {
    let run = read_json::<RunRecord>(data, keys::run())?;
    let start = read_json::<StartRecord>(data, keys::start())?;
    let status = read_json::<RunStatusRecord>(data, keys::status())?;
    let conclusion = read_json::<Conclusion>(data, keys::conclusion())?;

    let workflow_name = run.as_ref().map(|run| {
        if run.graph.name.is_empty() {
            "unnamed".to_string()
        } else {
            run.graph.name.clone()
        }
    });
    let goal = run.as_ref().and_then(|run| {
        let goal = run.graph.goal();
        (!goal.is_empty()).then(|| goal.to_string())
    });

    Ok(RunSummary {
        run_id: record.run_id.clone(),
        created_at: record.created_at,
        db_prefix: record.db_prefix.clone(),
        workflow_name,
        workflow_slug: run.as_ref().and_then(|run| run.workflow_slug.clone()),
        goal,
        labels: run
            .as_ref()
            .map(|run| run.labels.clone())
            .unwrap_or_default(),
        host_repo_path: run.as_ref().and_then(|run| run.host_repo_path.clone()),
        start_time: start.map(|start| start.start_time),
        status: status.as_ref().map(|status| status.status),
        status_reason: status.and_then(|status| status.reason),
        duration_ms: conclusion.as_ref().map(|conclusion| conclusion.duration_ms),
        total_cost: conclusion.and_then(|conclusion| conclusion.total_cost),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::Duration as ChronoDuration;
    use fabro_types::{AttrValue, FabroSettings, Graph, RunStatus, StageStatus, StatusReason};
    fn dt(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn sample_run_record(run_id: &str, created_at: DateTime<Utc>) -> RunRecord {
        let mut graph = Graph::new("night-sky");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("map the constellations".to_string()),
        );
        RunRecord {
            run_id: run_id.to_string(),
            created_at,
            settings: FabroSettings::default(),
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
            run_id: run_id.to_string(),
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
            run_id: run_id.to_string(),
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
            data_host: None,
        }
    }

    fn sample_node_status() -> NodeStatusRecord {
        NodeStatusRecord {
            status: StageStatus::PartialSuccess,
            notes: Some("captured output".to_string()),
            failure_reason: Some("minor lint".to_string()),
            timestamp: dt("2026-03-27T12:12:00Z"),
        }
    }

    #[tokio::test]
    async fn create_run_put_get_and_snapshot_round_trip() {
        let store = InMemoryStore::default();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store.create_run("run-1", created_at).await.unwrap();

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
        let node_status = sample_node_status();

        run.put_run(&run_record).await.unwrap();
        run.put_start(&start_record).await.unwrap();
        run.put_status(&status_record).await.unwrap();
        run.put_checkpoint(&checkpoint).await.unwrap();
        run.put_conclusion(&conclusion).await.unwrap();
        run.put_retro(&retro).await.unwrap();
        run.put_graph("digraph night_sky {}").await.unwrap();
        run.put_sandbox(&sandbox).await.unwrap();
        run.put_node_prompt(&node, "Plan the fix").await.unwrap();
        run.put_node_response(&node, "Implemented").await.unwrap();
        run.put_node_status(&node, &node_status).await.unwrap();
        run.put_node_stdout(&node, "ok").await.unwrap();
        run.put_node_stderr(&node, "").await.unwrap();
        run.put_retro_prompt("How did it go?").await.unwrap();
        run.put_retro_response("Smooth enough").await.unwrap();
        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
            .await
            .unwrap();
        run.put_asset(&node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let stored_run = run.get_run().await.unwrap().unwrap();
        assert_eq!(stored_run.run_id, run_record.run_id);
        assert_eq!(stored_run.created_at, run_record.created_at);
        assert_eq!(stored_run.workflow_slug, run_record.workflow_slug);
        assert_eq!(stored_run.graph.name, run_record.graph.name);

        let stored_start = run.get_start().await.unwrap().unwrap();
        assert_eq!(stored_start.run_id, start_record.run_id);
        assert_eq!(stored_start.start_time, start_record.start_time);

        let stored_status = run.get_status().await.unwrap().unwrap();
        assert_eq!(stored_status.status, status_record.status);
        assert_eq!(stored_status.reason, status_record.reason);

        let stored_checkpoint = run.get_checkpoint().await.unwrap().unwrap();
        assert_eq!(stored_checkpoint.current_node, checkpoint.current_node);
        assert_eq!(stored_checkpoint.next_node_id, checkpoint.next_node_id);

        let stored_conclusion = run.get_conclusion().await.unwrap().unwrap();
        assert_eq!(stored_conclusion.status, conclusion.status);
        assert_eq!(stored_conclusion.duration_ms, conclusion.duration_ms);
        assert_eq!(stored_conclusion.total_cost, conclusion.total_cost);

        let stored_retro = run.get_retro().await.unwrap().unwrap();
        assert_eq!(stored_retro.run_id, retro.run_id);
        assert_eq!(stored_retro.intent, retro.intent);
        assert_eq!(
            run.get_graph().await.unwrap(),
            Some("digraph night_sky {}".to_string())
        );
        let stored_sandbox = run.get_sandbox().await.unwrap().unwrap();
        assert_eq!(stored_sandbox.provider, sandbox.provider);
        assert_eq!(stored_sandbox.working_directory, sandbox.working_directory);
        assert_eq!(
            run.get_retro_prompt().await.unwrap(),
            Some("How did it go?".to_string())
        );
        assert_eq!(
            run.get_retro_response().await.unwrap(),
            Some("Smooth enough".to_string())
        );
        assert_eq!(
            run.get_artifact_value("summary").await.unwrap(),
            Some(serde_json::json!({"done": true}))
        );
        assert_eq!(
            run.get_asset(&node, "src/lib.rs").await.unwrap(),
            Some(Bytes::from_static(b"fn main() {}"))
        );
        assert_eq!(
            run.list_assets(&node).await.unwrap(),
            vec!["src/lib.rs".to_string()]
        );

        let snapshot = run.get_snapshot().await.unwrap().unwrap();
        assert_eq!(snapshot.run.run_id, run_record.run_id);
        assert_eq!(snapshot.run.created_at, run_record.created_at);
        assert_eq!(
            snapshot
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.current_node.as_str()),
            Some("code")
        );
        assert_eq!(
            snapshot
                .conclusion
                .as_ref()
                .map(|conclusion| conclusion.duration_ms),
            Some(3210)
        );
        assert_eq!(snapshot.nodes.len(), 1);
        assert_eq!(snapshot.nodes[0].node_id, "code");
        assert_eq!(snapshot.nodes[0].visit, 2);
        let snapshot_status = snapshot.nodes[0].status.as_ref().unwrap();
        assert_eq!(snapshot_status.status, node_status.status);
        assert_eq!(snapshot_status.failure_reason, node_status.failure_reason);
    }

    #[tokio::test]
    async fn append_event_validates_payload_shape_and_run_id() {
        let store = InMemoryStore::default();
        let run = store
            .create_run("run-1", dt("2026-03-27T12:00:00Z"))
            .await
            .unwrap();

        let invalid_missing: EventPayload = serde_json::from_value(serde_json::json!({
            "run_id": "run-1"
        }))
        .unwrap();
        let err = run.append_event(&invalid_missing).await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidEvent(_)));

        let invalid_run_id: EventPayload = serde_json::from_value(serde_json::json!({
            "ts": "2026-03-27T12:00:00Z",
            "run_id": "other-run",
            "event": "StageStarted"
        }))
        .unwrap();
        let err = run.append_event(&invalid_run_id).await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidEvent(_)));
    }

    #[tokio::test]
    async fn put_run_rejects_created_at_mismatch() {
        let store = InMemoryStore::default();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store.create_run("run-1", created_at).await.unwrap();
        let err = run
            .put_run(&sample_run_record(
                "run-1",
                created_at + ChronoDuration::minutes(1),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Other(_)));
    }

    #[tokio::test]
    async fn watch_events_from_receives_existing_and_live_events() {
        let store = InMemoryStore::default();
        let run = store
            .create_run("run-1", dt("2026-03-27T12:00:00Z"))
            .await
            .unwrap();
        let first = EventPayload::new(
            serde_json::json!({
                "ts": "2026-03-27T12:00:00.000Z",
                "run_id": "run-1",
                "event": "WorkflowRunStarted"
            }),
            "run-1",
        )
        .unwrap();
        let second = EventPayload::new(
            serde_json::json!({
                "ts": "2026-03-27T12:00:01.000Z",
                "run_id": "run-1",
                "event": "StageCompleted"
            }),
            "run-1",
        )
        .unwrap();

        run.append_event(&first).await.unwrap();

        let mut stream = run.watch_events_from(1).await.unwrap();
        let existing = tokio::time::timeout(
            Duration::from_secs(1),
            futures::StreamExt::next(&mut stream),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(existing.seq, 1);

        run.append_event(&second).await.unwrap();
        let live = tokio::time::timeout(
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
    async fn checkpoint_history_round_trips() {
        let store = InMemoryStore::default();
        let run = store
            .create_run("run-1", dt("2026-03-27T12:00:00Z"))
            .await
            .unwrap();
        let checkpoint = sample_checkpoint();
        let seq = run.append_checkpoint(&checkpoint).await.unwrap();
        assert_eq!(seq, 1);
        let checkpoints = run.list_checkpoints().await.unwrap();
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].0, 1);
        assert_eq!(checkpoints[0].1.current_node, checkpoint.current_node);

        let latest = run.get_checkpoint().await.unwrap().unwrap();
        assert_eq!(latest.current_node, checkpoint.current_node);
    }

    #[tokio::test]
    async fn node_visit_storage_round_trips() {
        let store = InMemoryStore::default();
        let run = store
            .create_run("run-1", dt("2026-03-27T12:00:00Z"))
            .await
            .unwrap();

        let first = NodeVisitRef {
            node_id: "code",
            visit: 1,
        };
        let second = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        run.put_node_prompt(&first, "first").await.unwrap();
        run.put_node_prompt(&second, "second").await.unwrap();
        run.put_node_status(&second, &sample_node_status())
            .await
            .unwrap();

        let node = run.get_node(&second).await.unwrap();
        assert_eq!(node.prompt, Some("second".to_string()));
        assert_eq!(run.list_node_visits("code").await.unwrap(), vec![1, 2]);
    }

    #[tokio::test]
    async fn list_runs_filters_dates_and_tolerates_missing_status() {
        let store = InMemoryStore::default();
        let early = dt("2026-03-27T10:00:00Z");
        let late = dt("2026-03-27T12:00:00Z");

        let early_run = store.create_run("run-early", early).await.unwrap();
        early_run
            .put_run(&sample_run_record("run-early", early))
            .await
            .unwrap();

        let late_run = store.create_run("run-late", late).await.unwrap();
        late_run
            .put_run(&sample_run_record("run-late", late))
            .await
            .unwrap();
        late_run
            .put_start(&sample_start_record("run-late", late))
            .await
            .unwrap();
        late_run
            .put_status(&sample_status(
                RunStatus::Succeeded,
                Some(StatusReason::Completed),
            ))
            .await
            .unwrap();
        late_run.put_conclusion(&sample_conclusion()).await.unwrap();

        let all = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].run_id, "run-late");
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
        assert_eq!(filtered[0].run_id, "run-late");
    }

    #[tokio::test]
    async fn delete_run_is_idempotent() {
        let store = InMemoryStore::default();
        store
            .create_run("run-1", dt("2026-03-27T12:00:00Z"))
            .await
            .unwrap();
        store.delete_run("run-1").await.unwrap();
        store.delete_run("run-1").await.unwrap();
        assert!(store.open_run("run-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn create_run_allows_retry_and_rejects_conflict() {
        let store = InMemoryStore::default();
        let ts = dt("2026-03-27T12:00:00Z");

        // First create succeeds.
        store.create_run("run-1", ts).await.unwrap();

        // Retry with exact same created_at succeeds (idempotent).
        store.create_run("run-1", ts).await.unwrap();

        // Different created_at for the same run_id is rejected.
        let different_ts = dt("2026-03-27T12:00:01Z");
        match store.create_run("run-1", different_ts).await {
            Err(StoreError::RunAlreadyExists(_)) => {} // expected
            Err(other) => panic!("expected RunAlreadyExists, got: {other:?}"),
            Ok(_) => panic!("expected RunAlreadyExists, but create_run succeeded"),
        }
    }
}
