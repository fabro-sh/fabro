use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use tracing::warn;

use crate::{
    EventEnvelope, EventPayload, NodeSnapshot, NodeVisitRef, Result, RunSnapshot, RunStore,
};
use fabro_types::{
    Checkpoint, Conclusion, NodeStatusRecord, Retro, RunRecord, RunStatusRecord, SandboxRecord,
    StartRecord,
};

#[derive(Debug, Clone)]
pub struct ProjectionError {
    pub path: PathBuf,
    pub critical: bool,
    pub error: String,
}

pub struct DiskProjectingRunStore {
    inner: Arc<dyn RunStore>,
    run_dir: PathBuf,
    on_projection_error: Option<Arc<dyn Fn(ProjectionError) + Send + Sync>>,
}

impl DiskProjectingRunStore {
    #[must_use]
    pub fn new(inner: Arc<dyn RunStore>, run_dir: PathBuf) -> Self {
        Self {
            inner,
            run_dir,
            on_projection_error: None,
        }
    }

    #[must_use]
    pub fn on_projection_error(
        mut self,
        callback: Arc<dyn Fn(ProjectionError) + Send + Sync>,
    ) -> Self {
        self.on_projection_error = Some(callback);
        self
    }

    fn report_projection_error(&self, path: &Path, err: &std::io::Error, critical: bool) {
        if critical {
            warn!(
                path = %path.display(),
                error = %err,
                "Critical disk projection failed"
            );
        } else {
            warn!(path = %path.display(), error = %err, "Disk projection failed");
        }

        if let Some(ref callback) = self.on_projection_error {
            callback(ProjectionError {
                path: path.to_path_buf(),
                critical,
                error: err.to_string(),
            });
        }
    }

    fn write_json_critical<T: serde::Serialize>(&self, path: &Path, value: &T) {
        if let Err(err) = write_json(path, value) {
            self.report_projection_error(path, &err, true);
        }
    }

    fn write_json_best_effort<T: serde::Serialize>(&self, path: &Path, value: &T) {
        if let Err(err) = write_json(path, value) {
            self.report_projection_error(path, &err, false);
        }
    }

    fn write_text_best_effort(&self, path: &Path, value: &str) {
        if let Err(err) = write_text(path, value) {
            self.report_projection_error(path, &err, false);
        }
    }

    fn append_jsonl_critical(&self, payload: &EventPayload) {
        let progress_path = self.run_dir.join("progress.jsonl");
        if let Err(err) = append_jsonl(&progress_path, payload) {
            self.report_projection_error(&progress_path, &err, true);
        }

        let live_path = self.run_dir.join("live.json");
        if let Err(err) = write_live_json(&live_path, payload) {
            self.report_projection_error(&live_path, &err, true);
        }
    }
}

/// Map store node visits onto the legacy on-disk layout used by workflow logs.
///
/// The store key layout uses `nodes/{id}/visit-{N}/...`, but existing disk readers
/// expect first visits at `nodes/{id}/...` and later visits at
/// `nodes/{id}-visit_{N}/...`.
fn disk_node_dir(run_dir: &Path, node_id: &str, visit: u32) -> PathBuf {
    if visit <= 1 {
        run_dir.join("nodes").join(node_id)
    } else {
        run_dir
            .join("nodes")
            .join(format!("{node_id}-visit_{visit}"))
    }
}

fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    ensure_parent_dir(path)?;
    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    fs::write(path, json)
}

fn write_text(path: &Path, value: &str) -> std::io::Result<()> {
    ensure_parent_dir(path)?;
    fs::write(path, value)
}

fn append_jsonl(path: &Path, payload: &EventPayload) -> std::io::Result<()> {
    ensure_parent_dir(path)?;
    let line = serde_json::to_string(payload.as_value()).map_err(std::io::Error::other)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

fn write_live_json(path: &Path, payload: &EventPayload) -> std::io::Result<()> {
    ensure_parent_dir(path)?;
    let json = serde_json::to_string_pretty(payload.as_value()).map_err(std::io::Error::other)?;
    fs::write(path, json)
}

#[async_trait]
impl RunStore for DiskProjectingRunStore {
    async fn put_run(&self, record: &RunRecord) -> Result<()> {
        self.inner.put_run(record).await?;
        self.write_json_best_effort(&self.run_dir.join("run.json"), record);
        Ok(())
    }

    async fn get_run(&self) -> Result<Option<RunRecord>> {
        self.inner.get_run().await
    }

    async fn put_start(&self, record: &StartRecord) -> Result<()> {
        self.inner.put_start(record).await?;
        self.write_json_best_effort(&self.run_dir.join("start.json"), record);
        Ok(())
    }

    async fn get_start(&self) -> Result<Option<StartRecord>> {
        self.inner.get_start().await
    }

    async fn put_status(&self, record: &RunStatusRecord) -> Result<()> {
        self.write_json_critical(&self.run_dir.join("status.json"), record);
        self.inner.put_status(record).await
    }

    async fn get_status(&self) -> Result<Option<RunStatusRecord>> {
        self.inner.get_status().await
    }

    async fn put_checkpoint(&self, record: &Checkpoint) -> Result<()> {
        self.inner.put_checkpoint(record).await?;
        self.write_json_best_effort(&self.run_dir.join("checkpoint.json"), record);
        Ok(())
    }

    async fn get_checkpoint(&self) -> Result<Option<Checkpoint>> {
        self.inner.get_checkpoint().await
    }

    async fn append_checkpoint(&self, record: &Checkpoint) -> Result<u32> {
        self.inner.append_checkpoint(record).await
    }

    async fn list_checkpoints(&self) -> Result<Vec<(u32, Checkpoint)>> {
        self.inner.list_checkpoints().await
    }

    async fn put_conclusion(&self, record: &Conclusion) -> Result<()> {
        self.write_json_critical(&self.run_dir.join("conclusion.json"), record);
        self.inner.put_conclusion(record).await
    }

    async fn get_conclusion(&self) -> Result<Option<Conclusion>> {
        self.inner.get_conclusion().await
    }

    async fn put_retro(&self, retro: &Retro) -> Result<()> {
        self.inner.put_retro(retro).await?;
        self.write_json_best_effort(&self.run_dir.join("retro.json"), retro);
        Ok(())
    }

    async fn get_retro(&self) -> Result<Option<Retro>> {
        self.inner.get_retro().await
    }

    async fn put_graph(&self, dot_source: &str) -> Result<()> {
        self.inner.put_graph(dot_source).await?;
        self.write_text_best_effort(&self.run_dir.join("workflow.fabro"), dot_source);
        Ok(())
    }

    async fn get_graph(&self) -> Result<Option<String>> {
        self.inner.get_graph().await
    }

    async fn put_sandbox(&self, record: &SandboxRecord) -> Result<()> {
        self.inner.put_sandbox(record).await?;
        self.write_json_best_effort(&self.run_dir.join("sandbox.json"), record);
        Ok(())
    }

    async fn get_sandbox(&self) -> Result<Option<SandboxRecord>> {
        self.inner.get_sandbox().await
    }

    async fn put_node_prompt(&self, node: &NodeVisitRef<'_>, prompt: &str) -> Result<()> {
        self.inner.put_node_prompt(node, prompt).await?;
        self.write_text_best_effort(
            &disk_node_dir(&self.run_dir, node.node_id, node.visit).join("prompt.md"),
            prompt,
        );
        Ok(())
    }

    async fn put_node_response(&self, node: &NodeVisitRef<'_>, response: &str) -> Result<()> {
        self.inner.put_node_response(node, response).await?;
        self.write_text_best_effort(
            &disk_node_dir(&self.run_dir, node.node_id, node.visit).join("response.md"),
            response,
        );
        Ok(())
    }

    async fn put_node_status(
        &self,
        node: &NodeVisitRef<'_>,
        status: &NodeStatusRecord,
    ) -> Result<()> {
        self.inner.put_node_status(node, status).await?;
        self.write_json_best_effort(
            &disk_node_dir(&self.run_dir, node.node_id, node.visit).join("status.json"),
            status,
        );
        Ok(())
    }

    async fn put_node_stdout(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()> {
        self.inner.put_node_stdout(node, log).await?;
        self.write_text_best_effort(
            &disk_node_dir(&self.run_dir, node.node_id, node.visit).join("stdout.log"),
            log,
        );
        Ok(())
    }

    async fn put_node_stderr(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()> {
        self.inner.put_node_stderr(node, log).await?;
        self.write_text_best_effort(
            &disk_node_dir(&self.run_dir, node.node_id, node.visit).join("stderr.log"),
            log,
        );
        Ok(())
    }

    async fn get_node(&self, node: &NodeVisitRef<'_>) -> Result<NodeSnapshot> {
        self.inner.get_node(node).await
    }

    async fn list_node_visits(&self, node_id: &str) -> Result<Vec<u32>> {
        self.inner.list_node_visits(node_id).await
    }

    async fn append_event(&self, payload: &EventPayload) -> Result<u32> {
        self.append_jsonl_critical(payload);
        self.inner.append_event(payload).await
    }

    async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        self.inner.list_events().await
    }

    async fn list_events_from(&self, seq: u32) -> Result<Vec<EventEnvelope>> {
        self.inner.list_events_from(seq).await
    }

    async fn watch_events_from(
        &self,
        seq: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<EventEnvelope>> + Send>>> {
        self.inner.watch_events_from(seq).await
    }

    async fn put_retro_prompt(&self, text: &str) -> Result<()> {
        self.inner.put_retro_prompt(text).await?;
        self.write_text_best_effort(&self.run_dir.join("retro").join("prompt.md"), text);
        Ok(())
    }

    async fn get_retro_prompt(&self) -> Result<Option<String>> {
        self.inner.get_retro_prompt().await
    }

    async fn put_retro_response(&self, text: &str) -> Result<()> {
        self.inner.put_retro_response(text).await?;
        self.write_text_best_effort(&self.run_dir.join("retro").join("response.md"), text);
        Ok(())
    }

    async fn get_retro_response(&self) -> Result<Option<String>> {
        self.inner.get_retro_response().await
    }

    async fn put_artifact_value(&self, artifact_id: &str, value: &serde_json::Value) -> Result<()> {
        self.inner.put_artifact_value(artifact_id, value).await
    }

    async fn get_artifact_value(&self, artifact_id: &str) -> Result<Option<serde_json::Value>> {
        self.inner.get_artifact_value(artifact_id).await
    }

    async fn list_artifact_values(&self) -> Result<Vec<String>> {
        self.inner.list_artifact_values().await
    }

    async fn put_asset(&self, node: &NodeVisitRef<'_>, filename: &str, data: &[u8]) -> Result<()> {
        self.inner.put_asset(node, filename, data).await
    }

    async fn get_asset(&self, node: &NodeVisitRef<'_>, filename: &str) -> Result<Option<Bytes>> {
        self.inner.get_asset(node, filename).await
    }

    async fn list_assets(&self, node: &NodeVisitRef<'_>) -> Result<Vec<String>> {
        self.inner.list_assets(node).await
    }

    async fn list_all_assets(&self) -> Result<Vec<(String, u32, String)>> {
        self.inner.list_all_assets().await
    }

    async fn get_snapshot(&self) -> Result<Option<RunSnapshot>> {
        self.inner.get_snapshot().await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use chrono::{DateTime, Duration as ChronoDuration, Utc};
    use tempfile::TempDir;

    use super::*;
    use crate::{InMemoryStore, Store};
    use fabro_types::{
        AggregateStats, AttrValue, FabroSettings, Graph, RunId, RunStatus, StageStatus,
        StatusReason, fixtures,
    };

    fn dt(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn test_run_id(label: &str) -> RunId {
        match label {
            "run-1" => fixtures::RUN_1,
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
            stats: AggregateStats {
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

    fn sample_node_status() -> NodeStatusRecord {
        NodeStatusRecord {
            status: StageStatus::PartialSuccess,
            notes: Some("captured output".to_string()),
            failure_reason: Some("minor lint".to_string()),
            timestamp: dt("2026-03-27T12:12:00Z"),
        }
    }

    fn event_payload(run_id: &str, ts: &str, event: &str) -> EventPayload {
        EventPayload::new(
            serde_json::json!({
                "id": format!("evt-{run_id}-{event}"),
                "ts": ts,
                "run_id": test_run_id(run_id).to_string(),
                "event": event,
            }),
            &test_run_id(run_id),
        )
        .unwrap()
    }

    async fn make_store(
        run_dir: &Path,
        created_at: DateTime<Utc>,
    ) -> (Arc<dyn RunStore>, DiskProjectingRunStore) {
        let inner = InMemoryStore::default()
            .create_run(
                &test_run_id("run-1"),
                created_at,
                Some(run_dir.to_string_lossy().as_ref()),
            )
            .await
            .unwrap();
        let projected = DiskProjectingRunStore::new(Arc::clone(&inner), run_dir.to_path_buf());
        (inner, projected)
    }

    #[tokio::test]
    async fn put_methods_project_expected_files() {
        let temp = TempDir::new().unwrap();
        let created_at = dt("2026-03-27T12:00:00Z");
        let (_inner, store) = make_store(temp.path(), created_at).await;

        let run = sample_run_record("run-1", created_at);
        let start = sample_start_record("run-1", created_at);
        let status = sample_status(RunStatus::Running, Some(StatusReason::SandboxInitializing));
        let checkpoint = sample_checkpoint();
        let conclusion = sample_conclusion();
        let retro = sample_retro("run-1");
        let sandbox = sample_sandbox();
        let node_status = sample_node_status();

        store.put_run(&run).await.unwrap();
        store.put_start(&start).await.unwrap();
        store.put_status(&status).await.unwrap();
        store.put_checkpoint(&checkpoint).await.unwrap();
        store.put_conclusion(&conclusion).await.unwrap();
        store.put_retro(&retro).await.unwrap();
        store.put_graph("digraph night_sky {}").await.unwrap();
        store.put_sandbox(&sandbox).await.unwrap();

        let visit_one = NodeVisitRef {
            node_id: "code",
            visit: 1,
        };
        store
            .put_node_response(&visit_one, "Applied the fix")
            .await
            .unwrap();
        store
            .put_node_status(&visit_one, &node_status)
            .await
            .unwrap();
        store.put_node_stdout(&visit_one, "stdout").await.unwrap();
        store.put_node_stderr(&visit_one, "stderr").await.unwrap();

        let visit_two = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        store
            .put_node_prompt(&visit_two, "Plan the fix")
            .await
            .unwrap();
        store.put_retro_prompt("How did it go?").await.unwrap();
        store.put_retro_response("Smooth enough").await.unwrap();

        assert_eq!(
            serde_json::to_value(
                serde_json::from_str::<RunRecord>(
                    &fs::read_to_string(temp.path().join("run.json")).unwrap()
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::to_value(run).unwrap()
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_str::<StartRecord>(
                    &fs::read_to_string(temp.path().join("start.json")).unwrap()
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::to_value(start).unwrap()
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_str::<RunStatusRecord>(
                    &fs::read_to_string(temp.path().join("status.json")).unwrap()
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::to_value(status).unwrap()
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_str::<Checkpoint>(
                    &fs::read_to_string(temp.path().join("checkpoint.json")).unwrap()
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::to_value(checkpoint).unwrap()
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_str::<Conclusion>(
                    &fs::read_to_string(temp.path().join("conclusion.json")).unwrap()
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::to_value(conclusion).unwrap()
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_str::<Retro>(
                    &fs::read_to_string(temp.path().join("retro.json")).unwrap()
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::to_value(retro).unwrap()
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("workflow.fabro")).unwrap(),
            "digraph night_sky {}"
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_str::<SandboxRecord>(
                    &fs::read_to_string(temp.path().join("sandbox.json")).unwrap()
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::to_value(sandbox).unwrap()
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("nodes/code/response.md")).unwrap(),
            "Applied the fix"
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_str::<NodeStatusRecord>(
                    &fs::read_to_string(temp.path().join("nodes/code/status.json")).unwrap()
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::to_value(node_status).unwrap()
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("nodes/code/stdout.log")).unwrap(),
            "stdout"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("nodes/code/stderr.log")).unwrap(),
            "stderr"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("nodes/code-visit_2/prompt.md")).unwrap(),
            "Plan the fix"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("retro/prompt.md")).unwrap(),
            "How did it go?"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("retro/response.md")).unwrap(),
            "Smooth enough"
        );
    }

    #[tokio::test]
    async fn critical_projections_write_files_in_isolation() {
        let temp = TempDir::new().unwrap();
        let created_at = dt("2026-03-27T12:00:00Z");
        let (_inner, store) = make_store(temp.path(), created_at).await;

        let status = sample_status(RunStatus::Failed, Some(StatusReason::WorkflowError));
        let conclusion = sample_conclusion();

        store.put_status(&status).await.unwrap();
        store.put_conclusion(&conclusion).await.unwrap();

        assert!(temp.path().join("status.json").exists());
        assert!(temp.path().join("conclusion.json").exists());
    }

    #[tokio::test]
    async fn append_event_projects_progress_and_live_files() {
        let temp = TempDir::new().unwrap();
        let created_at = dt("2026-03-27T12:00:00Z");
        let (_inner, store) = make_store(temp.path(), created_at).await;

        let first = event_payload("run-1", "2026-03-27T12:00:00Z", "Started");
        let second = event_payload("run-1", "2026-03-27T12:00:01Z", "Completed");

        store.append_event(&first).await.unwrap();
        store.append_event(&second).await.unwrap();

        let progress = fs::read_to_string(temp.path().join("progress.jsonl")).unwrap();
        let lines: Vec<&str> = progress.lines().collect();
        assert_eq!(lines.len(), 2);
        let first_value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let second_value: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first_value, first.as_value().clone());
        assert_eq!(second_value, second.as_value().clone());

        let live: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(temp.path().join("live.json")).unwrap())
                .unwrap();
        assert_eq!(live, second.as_value().clone());

        let events = store.list_events().await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].payload, first);
        assert_eq!(events[1].payload, second);
    }

    #[tokio::test]
    async fn get_methods_read_from_inner_store_not_disk() {
        let temp = TempDir::new().unwrap();
        let created_at = dt("2026-03-27T12:00:00Z");
        let (inner, store) = make_store(temp.path(), created_at).await;

        let status = sample_status(RunStatus::Running, Some(StatusReason::SandboxInitializing));
        inner.put_status(&status).await.unwrap();
        fs::write(
            temp.path().join("status.json"),
            serde_json::to_string_pretty(&sample_status(
                RunStatus::Succeeded,
                Some(StatusReason::Completed),
            ))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(store.get_status().await.unwrap().unwrap()).unwrap(),
            serde_json::to_value(status).unwrap()
        );
    }

    #[tokio::test]
    async fn disk_failures_do_not_block_store_writes() {
        let temp = TempDir::new().unwrap();
        let created_at = dt("2026-03-27T12:00:00Z");
        let (inner, store) = make_store(temp.path(), created_at).await;

        let mut permissions = fs::metadata(temp.path()).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(temp.path(), permissions).unwrap();

        let status = sample_status(RunStatus::Running, Some(StatusReason::SandboxInitializing));
        let node = NodeVisitRef {
            node_id: "code",
            visit: 1,
        };

        store.put_status(&status).await.unwrap();
        store.put_node_prompt(&node, "Plan the fix").await.unwrap();

        let stored_status = inner.get_status().await.unwrap();
        let stored_node = inner.get_node(&node).await.unwrap();

        assert_eq!(
            serde_json::to_value(stored_status.unwrap()).unwrap(),
            serde_json::to_value(status).unwrap()
        );
        assert_eq!(stored_node.prompt.as_deref(), Some("Plan the fix"));
    }

    #[tokio::test]
    async fn projection_error_callback_runs_on_disk_failure() {
        let temp = TempDir::new().unwrap();
        let created_at = dt("2026-03-27T12:00:00Z");
        let inner = InMemoryStore::default()
            .create_run(
                &test_run_id("run-1"),
                created_at,
                Some(temp.path().to_string_lossy().as_ref()),
            )
            .await
            .unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<ProjectionError>::new()));
        let seen_clone = Arc::clone(&seen);
        let store = DiskProjectingRunStore::new(inner, temp.path().to_path_buf())
            .on_projection_error(Arc::new(move |error| {
                seen_clone.lock().unwrap().push(error);
            }));

        let mut permissions = fs::metadata(temp.path()).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(temp.path(), permissions).unwrap();

        store
            .put_status(&sample_status(
                RunStatus::Running,
                Some(StatusReason::SandboxInitializing),
            ))
            .await
            .unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].critical);
        assert_eq!(seen[0].path, temp.path().join("status.json"));
        assert!(!seen[0].error.is_empty());
    }

    #[test]
    fn disk_node_dir_matches_legacy_layout() {
        let run_dir = Path::new("/tmp/fabro-run");

        assert_eq!(
            disk_node_dir(run_dir, "build", 1),
            run_dir.join("nodes/build")
        );
        assert_eq!(
            disk_node_dir(run_dir, "build", 2),
            run_dir.join("nodes/build-visit_2")
        );
    }
}
