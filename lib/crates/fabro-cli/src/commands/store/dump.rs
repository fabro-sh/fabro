use anyhow::{Context, Result};
use bytes::Bytes;
#[cfg(test)]
use fabro_store::{ArtifactStore, RunDatabase};
use fabro_store::{EventEnvelope, RunProjection, StageId};
use fabro_types::{RunBlobId, RunId};
use fabro_workflow::run_dump::RunDump;
use futures::future::BoxFuture;
#[cfg(test)]
use serde::de::DeserializeOwned;
use std::io::ErrorKind;
use std::path::Path;

use crate::args::{GlobalArgs, StoreDumpArgs};
use crate::server_client::ServerStoreClient;
use crate::server_runs::ServerRunLookup;
use crate::shared::{absolute_or_current, print_json_pretty};
use crate::user_config::load_settings_with_storage_dir;

pub(crate) async fn dump_command(args: &StoreDumpArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
    let lookup = ServerRunLookup::connect(&cli_settings.storage_dir()).await?;
    let run = lookup.resolve(&args.run)?;
    let run_id = run.run_id();
    let state = lookup.client().get_run_state(&run_id).await?;
    let source = ServerDumpSource::new(lookup.client(), &run_id);
    let file_count = export_run_from_source(&source, &state, &args.output).await?;
    if globals.json {
        print_json_pretty(&serde_json::json!({
            "run_id": run_id,
            "output_dir": absolute_or_current(&args.output),
            "file_count": file_count,
        }))?;
    } else {
        println!(
            "Exported {file_count} files for run {} to {}",
            run_id,
            args.output.display()
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn export_run(
    run_store: &RunDatabase,
    artifact_store: &ArtifactStore,
    output_dir: &Path,
) -> Result<usize> {
    let state = run_store.state().await?;
    let run_id = state
        .run
        .as_ref()
        .map(|run| run.run_id)
        .context("run has no data in the store")?;
    let source = LocalDumpSource::new(run_store, artifact_store, run_id);
    export_run_from_source(&source, &state, output_dir).await
}

fn finalize_export(
    output_dir: &Path,
    output_state: OutputDirState,
    staging_dir: tempfile::TempDir,
    staging_path: &Path,
    file_count: usize,
) -> Result<usize> {
    if matches!(output_state, OutputDirState::ExistingEmpty) {
        std::fs::remove_dir(output_dir)
            .with_context(|| format!("failed to replace {}", output_dir.display()))?;
    }
    std::fs::rename(staging_path, output_dir).with_context(|| {
        format!(
            "failed to move staged export {} into {}",
            staging_path.display(),
            output_dir.display()
        )
    })?;
    let _ = staging_dir.keep();

    Ok(file_count)
}

struct DumpArtifact {
    stage_id: StageId,
    relative_path: String,
    data: Vec<u8>,
}

trait DumpDataSource {
    fn list_events(&self) -> BoxFuture<'_, Result<Vec<EventEnvelope>>>;

    fn read_blob(&self, blob_id: RunBlobId) -> BoxFuture<'_, Result<Option<Bytes>>>;

    fn list_artifacts(&self) -> BoxFuture<'_, Result<Vec<DumpArtifact>>>;
}

#[cfg(test)]
struct LocalDumpSource<'a> {
    run_store: &'a RunDatabase,
    artifact_store: &'a ArtifactStore,
    run_id: RunId,
}

#[cfg(test)]
impl<'a> LocalDumpSource<'a> {
    fn new(run_store: &'a RunDatabase, artifact_store: &'a ArtifactStore, run_id: RunId) -> Self {
        Self {
            run_store,
            artifact_store,
            run_id,
        }
    }
}

#[cfg(test)]
impl DumpDataSource for LocalDumpSource<'_> {
    fn list_events(&self) -> BoxFuture<'_, Result<Vec<EventEnvelope>>> {
        Box::pin(async move { Ok(self.run_store.list_events().await?) })
    }

    fn read_blob(&self, blob_id: RunBlobId) -> BoxFuture<'_, Result<Option<Bytes>>> {
        Box::pin(async move { Ok(self.run_store.read_blob(&blob_id).await?) })
    }

    fn list_artifacts(&self) -> BoxFuture<'_, Result<Vec<DumpArtifact>>> {
        Box::pin(async move {
            let mut artifacts = Vec::new();
            for asset in self.artifact_store.list_for_run(&self.run_id).await? {
                let data = self
                    .artifact_store
                    .get(&self.run_id, &asset.node, &asset.filename)
                    .await?
                    .with_context(|| {
                        format!(
                            "asset {:?} for node {:?} visit {} is missing from the store",
                            asset.filename,
                            asset.node.node_id(),
                            asset.node.visit()
                        )
                    })?;
                artifacts.push(DumpArtifact {
                    stage_id: asset.node,
                    relative_path: asset.filename,
                    data: data.to_vec(),
                });
            }
            Ok(artifacts)
        })
    }
}

struct ServerDumpSource<'a> {
    client: &'a ServerStoreClient,
    run_id: &'a RunId,
}

impl<'a> ServerDumpSource<'a> {
    fn new(client: &'a ServerStoreClient, run_id: &'a RunId) -> Self {
        Self { client, run_id }
    }
}

impl DumpDataSource for ServerDumpSource<'_> {
    fn list_events(&self) -> BoxFuture<'_, Result<Vec<EventEnvelope>>> {
        Box::pin(async move { self.client.list_run_events(self.run_id, None, None).await })
    }

    fn read_blob(&self, blob_id: RunBlobId) -> BoxFuture<'_, Result<Option<Bytes>>> {
        Box::pin(async move { self.client.read_run_blob(self.run_id, &blob_id).await })
    }

    fn list_artifacts(&self) -> BoxFuture<'_, Result<Vec<DumpArtifact>>> {
        Box::pin(async move {
            let mut artifacts = Vec::new();
            for artifact in self.client.list_run_artifacts(self.run_id).await? {
                let stage_id: StageId = artifact.stage_id.parse().with_context(|| {
                    format!("server returned invalid stage id {:?}", artifact.stage_id)
                })?;
                let data = self
                    .client
                    .download_stage_artifact(self.run_id, &stage_id, &artifact.relative_path)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to download artifact {} for stage {}",
                            artifact.relative_path, artifact.stage_id
                        )
                    })?;
                artifacts.push(DumpArtifact {
                    stage_id,
                    relative_path: artifact.relative_path,
                    data,
                });
            }
            Ok(artifacts)
        })
    }
}

async fn export_run_from_source(
    source: &impl DumpDataSource,
    state: &RunProjection,
    output_dir: &Path,
) -> Result<usize> {
    let output_state = inspect_output_dir(output_dir)?;
    let staging_parent = output_parent_dir(output_dir);
    std::fs::create_dir_all(staging_parent)
        .with_context(|| format!("failed to create {}", staging_parent.display()))?;

    let staging_dir = tempfile::Builder::new()
        .prefix(".fabro-store-dump-")
        .tempdir_in(staging_parent)
        .with_context(|| {
            format!(
                "failed to create staging dir in {}",
                staging_parent.display()
            )
        })?;
    let staging_path = staging_dir.path().to_path_buf();

    let file_count = write_run_dump(source, state, &staging_path).await?;
    finalize_export(
        output_dir,
        output_state,
        staging_dir,
        &staging_path,
        file_count,
    )
}

async fn write_run_dump(
    source: &impl DumpDataSource,
    state: &RunProjection,
    output_dir: &Path,
) -> Result<usize> {
    let events = source.list_events().await?;
    let mut dump = RunDump::from_store_state_and_events(state, &events)?;

    dump.hydrate_referenced_blobs_with_reader(|blob_id| source.read_blob(blob_id))
        .await?;

    for artifact in source.list_artifacts().await? {
        dump.add_artifact_bytes(&artifact.stage_id, &artifact.relative_path, artifact.data)?;
    }

    dump.write_to_dir(output_dir)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputDirState {
    Missing,
    ExistingEmpty,
}

fn inspect_output_dir(path: &Path) -> Result<OutputDirState> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(output_dir_error(path));
            }

            let mut entries = std::fs::read_dir(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if entries.next().transpose()?.is_some() {
                return Err(output_dir_error(path));
            }

            Ok(OutputDirState::ExistingEmpty)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(OutputDirState::Missing),
        Err(err) => Err(err.into()),
    }
}

fn output_dir_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "output path {} already exists and is not an empty directory; remove it first or choose a different path",
        path.display()
    )
}

fn output_parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use fabro_store::{Database, EventEnvelope, EventPayload};
    use fabro_types::settings::SettingsFile;
    use fabro_types::{
        AggregateStats, AttrValue, BilledTokenCounts, Checkpoint, Conclusion, Graph,
        NodeStatusRecord, Retro, RunId, RunRecord, RunStatus, RunStatusRecord, SandboxRecord,
        StageStatus, StartRecord, StatusReason, fixtures,
    };
    use fabro_workflow::event::{Event, append_event};
    use object_store::{ObjectStore, memory::InMemory};

    fn dt(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn test_store_bundle() -> (Arc<Database>, ArtifactStore) {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = Arc::new(Database::new(
            Arc::clone(&object_store),
            "",
            Duration::from_millis(1),
        ));
        let artifact_store = ArtifactStore::new(object_store, "artifacts");
        (store, artifact_store)
    }

    fn sample_run_record(run_id: RunId, _created_at: DateTime<Utc>) -> RunRecord {
        let mut graph = Graph::new("night-sky");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("map the constellations".to_string()),
        );
        RunRecord {
            run_id,
            settings: SettingsFile::default(),
            graph,
            workflow_slug: Some("night-sky".to_string()),
            working_directory: PathBuf::from("/tmp/night-sky"),
            host_repo_path: Some("github.com/fabro-sh/fabro".to_string()),
            repo_origin_url: Some("https://github.com/fabro-sh/fabro".to_string()),
            base_branch: Some("main".to_string()),
            labels: HashMap::from([("team".to_string(), "infra".to_string())]),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
        }
    }

    fn sample_start_record(run_id: RunId, created_at: DateTime<Utc>) -> StartRecord {
        StartRecord {
            run_id,
            start_time: created_at + chrono::Duration::seconds(5),
            run_branch: Some(format!("fabro/run/{run_id}")),
            base_sha: Some("abc123".to_string()),
        }
    }

    fn sample_status() -> RunStatusRecord {
        RunStatusRecord {
            status: RunStatus::Running,
            reason: Some(StatusReason::SandboxInitializing),
            updated_at: dt("2026-03-27T12:05:00Z"),
        }
    }

    fn sample_checkpoint(current_node: &str, visit: u32) -> Checkpoint {
        Checkpoint {
            timestamp: dt("2026-03-27T12:10:00Z"),
            current_node: current_node.to_string(),
            completed_nodes: vec!["plan".to_string()],
            node_retries: HashMap::from([(current_node.to_string(), visit.saturating_sub(1))]),
            context_values: HashMap::from([(
                "artifact".to_string(),
                serde_json::json!({"kind": "summary"}),
            )]),
            node_outcomes: HashMap::new(),
            next_node_id: Some("review".to_string()),
            git_commit_sha: Some("def456".to_string()),
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits: HashMap::from([(current_node.to_string(), visit as usize)]),
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
            billing: Some(BilledTokenCounts {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 150,
                reasoning_tokens: 50,
                cache_read_tokens: 30,
                cache_write_tokens: 40,
                total_usd_micros: Some(1_250_000),
            }),
            total_retries: 2,
        }
    }

    fn sample_retro(run_id: RunId) -> Retro {
        Retro {
            run_id,
            workflow_name: "night-sky".to_string(),
            goal: "map the constellations".to_string(),
            timestamp: dt("2026-03-27T12:20:00Z"),
            smoothness: None,
            stages: Vec::new(),
            stats: AggregateStats {
                total_duration_ms: 3210,
                total_billing_usd_micros: Some(1_250_000),
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

    fn read_json<T: DeserializeOwned>(path: &Path) -> T {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()))
    }

    #[tokio::test]
    async fn export_run_writes_expected_directory_tree() {
        let (store, artifact_store) = test_store_bundle();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run_id = test_run_id();
        let run = store.create_run(&run_id).await.unwrap();
        let run_record = sample_run_record(run_id, created_at);
        let start_record = sample_start_record(run_id, created_at);
        let status_record = sample_status();
        let mut first_checkpoint = sample_checkpoint("plan", 1);
        let mut second_checkpoint = sample_checkpoint("code", 2);
        let conclusion = sample_conclusion();
        let retro = sample_retro(run_id);
        let sandbox = sample_sandbox();
        let summary_blob = run.write_blob(br#"{"done":true}"#).await.unwrap();
        let plan_blob = run.write_blob(br#"{"steps":3}"#).await.unwrap();
        first_checkpoint.context_values.insert(
            "artifact".to_string(),
            serde_json::json!(fabro_types::format_blob_ref(&plan_blob)),
        );
        second_checkpoint.context_values.insert(
            "artifact".to_string(),
            serde_json::json!(fabro_types::format_blob_ref(&summary_blob)),
        );

        let node = StageId::new("code", 2);
        append_event(
            &run,
            &run_id,
            &Event::RunCreated {
                run_id,
                settings: serde_json::to_value(&run_record.settings).unwrap(),
                graph: serde_json::to_value(&run_record.graph).unwrap(),
                workflow_source: Some("digraph night_sky {}".to_string()),
                workflow_config: None,
                labels: run_record.labels.clone().into_iter().collect(),
                run_dir: "/tmp/night-sky-run".to_string(),
                working_directory: run_record.working_directory.display().to_string(),
                host_repo_path: run_record.host_repo_path.clone(),
                repo_origin_url: run_record.repo_origin_url.clone(),
                base_branch: run_record.base_branch.clone(),
                workflow_slug: run_record.workflow_slug.clone(),
                db_prefix: None,
                provenance: run_record.provenance.clone(),
                manifest_blob: None,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::WorkflowRunStarted {
                name: "night-sky".to_string(),
                run_id,
                base_branch: run_record.base_branch.clone(),
                base_sha: start_record.base_sha.clone(),
                run_branch: start_record.run_branch.clone(),
                worktree_dir: None,
                goal: Some("map the constellations".to_string()),
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::RunRunning {
                reason: status_record.reason,
            },
        )
        .await
        .unwrap();
        for checkpoint in [&first_checkpoint, &second_checkpoint] {
            append_event(
                &run,
                &run_id,
                &Event::CheckpointCompleted {
                    node_id: checkpoint.current_node.clone(),
                    status: "success".to_string(),
                    current_node: checkpoint.current_node.clone(),
                    completed_nodes: checkpoint.completed_nodes.clone(),
                    node_retries: checkpoint.node_retries.clone().into_iter().collect(),
                    context_values: checkpoint.context_values.clone().into_iter().collect(),
                    node_outcomes: checkpoint.node_outcomes.clone().into_iter().collect(),
                    next_node_id: checkpoint.next_node_id.clone(),
                    git_commit_sha: checkpoint.git_commit_sha.clone(),
                    loop_failure_signatures: checkpoint
                        .loop_failure_signatures
                        .clone()
                        .into_iter()
                        .map(|(signature, count)| (signature.to_string(), count))
                        .collect(),
                    restart_failure_signatures: checkpoint
                        .restart_failure_signatures
                        .clone()
                        .into_iter()
                        .map(|(signature, count)| (signature.to_string(), count))
                        .collect(),
                    node_visits: checkpoint.node_visits.clone().into_iter().collect(),
                    diff: None,
                },
            )
            .await
            .unwrap();
        }
        append_event(
            &run,
            &run_id,
            &Event::SandboxInitialized {
                working_directory: sandbox.working_directory.clone(),
                provider: sandbox.provider.clone(),
                identifier: sandbox.identifier.clone(),
                host_working_directory: sandbox.host_working_directory.clone(),
                container_mount_point: sandbox.container_mount_point.clone(),
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::Prompt {
                stage: "code".to_string(),
                visit: 2,
                text: "Plan the fix".to_string(),
                mode: None,
                provider: None,
                model: None,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::PromptCompleted {
                node_id: "code".to_string(),
                response: "Implemented".to_string(),
                model: "gpt-5".to_string(),
                provider: "openai".to_string(),
                billing: None,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::StageCompleted {
                node_id: "code".to_string(),
                name: "Code".to_string(),
                index: 1,
                duration_ms: 250,
                status: "partial_success".to_string(),
                preferred_label: None,
                suggested_next_ids: Vec::new(),
                billing: None,
                failure: None,
                notes: Some("captured output".to_string()),
                files_touched: Vec::new(),
                context_updates: None,
                jump_to_node: None,
                context_values: None,
                node_visits: Some(std::collections::BTreeMap::from([(
                    "code".to_string(),
                    2usize,
                )])),
                loop_failure_signatures: None,
                restart_failure_signatures: None,
                response: Some("Implemented".to_string()),
                attempt: 1,
                max_attempts: 1,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::CommandStarted {
                node_id: "code".to_string(),
                script: "echo hi".to_string(),
                command: "echo hi".to_string(),
                language: "sh".to_string(),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::CommandCompleted {
                node_id: "code".to_string(),
                stdout: "stdout line".to_string(),
                stderr: String::new(),
                exit_code: Some(0),
                duration_ms: 100,
                timed_out: false,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::RetroStarted {
                prompt: Some("How did it go?".to_string()),
                provider: None,
                model: None,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::RetroCompleted {
                duration_ms: 50,
                response: Some("Smooth enough".to_string()),
                retro: Some(serde_json::to_value(&retro).unwrap()),
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &run_id,
            &Event::WorkflowRunCompleted {
                duration_ms: conclusion.duration_ms,
                artifact_count: 0,
                status: "success".to_string(),
                reason: None,
                total_usd_micros: conclusion
                    .billing
                    .as_ref()
                    .and_then(|billing| billing.total_usd_micros),
                final_git_commit_sha: conclusion.final_git_commit_sha.clone(),
                final_patch: None,
                billing: conclusion.billing.clone(),
            },
        )
        .await
        .unwrap();
        run.append_event(
            &EventPayload::new(
                serde_json::json!({
                    "id": format!("evt-{run_id}-stage-completed"),
                    "ts": "2026-03-27T12:00:01.000Z",
                    "run_id": run_id.to_string(),
                    "event": "stage.completed",
                    "node_id": "code",
                    "node_label": "Code",
                    "properties": {
                        "index": 1,
                        "duration_ms": 1,
                        "status": "success",
                        "response": "Implemented",
                        "notes": "all good",
                        "files_touched": ["src/lib.rs"],
                        "node_visits": {"code": 2},
                        "attempt": 1,
                        "max_attempts": 1
                    }
                }),
                &run_id,
            )
            .unwrap(),
        )
        .await
        .unwrap();
        artifact_store
            .put(&run_id, &node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let artifact_only_node = StageId::new("artifact-only", 7);
        artifact_store
            .put(&run_id, &artifact_only_node, "logs/output.txt", b"hello")
            .await
            .unwrap();

        let output = tempfile::tempdir().unwrap();
        let file_count = export_run(&run, &artifact_store, output.path())
            .await
            .unwrap();
        assert_eq!(file_count, 20);

        let exported_run: RunRecord = read_json(&output.path().join("run.json"));
        assert_eq!(exported_run.run_id, run_id);

        let exported_start: StartRecord = read_json(&output.path().join("start.json"));
        assert_eq!(exported_start.run_id, run_id);

        let exported_status: RunStatusRecord = read_json(&output.path().join("status.json"));
        assert_eq!(exported_status.status, RunStatus::Succeeded);

        let exported_checkpoint: Checkpoint = read_json(&output.path().join("checkpoint.json"));
        assert_eq!(exported_checkpoint.current_node, "code");
        assert_eq!(
            exported_checkpoint.context_values.get("artifact"),
            Some(&serde_json::json!({"done": true}))
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("graph.fabro")).unwrap(),
            "digraph night_sky {}"
        );

        assert_eq!(
            std::fs::read_to_string(output.path().join("nodes/code/visit-2/prompt.md")).unwrap(),
            "Plan the fix"
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("nodes/code/visit-2/response.md")).unwrap(),
            "Implemented"
        );
        let node_status: NodeStatusRecord =
            read_json(&output.path().join("nodes/code/visit-2/status.json"));
        assert_eq!(node_status.status, StageStatus::Success);
        assert_eq!(
            std::fs::read_to_string(output.path().join("nodes/code/visit-2/stdout.log")).unwrap(),
            "stdout line"
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("nodes/code/visit-2/stderr.log")).unwrap(),
            ""
        );

        assert_eq!(
            std::fs::read_to_string(output.path().join("retro/prompt.md")).unwrap(),
            "How did it go?"
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("retro/response.md")).unwrap(),
            "Smooth enough"
        );

        let event_lines = std::fs::read_to_string(output.path().join("events.jsonl")).unwrap();
        let events: Vec<EventEnvelope> = event_lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 15);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events.last().unwrap().seq, 15);

        let first_checkpoint: Checkpoint = read_json(&output.path().join("checkpoints/0004.json"));
        let second_checkpoint: Checkpoint = read_json(&output.path().join("checkpoints/0005.json"));
        assert_eq!(first_checkpoint.current_node, "plan");
        assert_eq!(second_checkpoint.current_node, "code");
        assert_eq!(
            first_checkpoint.context_values.get("artifact"),
            Some(&serde_json::json!({"steps": 3}))
        );
        assert_eq!(
            second_checkpoint.context_values.get("artifact"),
            Some(&serde_json::json!({"done": true}))
        );
        assert!(!output.path().join("blobs").exists());

        assert_eq!(
            std::fs::read(
                output
                    .path()
                    .join("artifacts/nodes/code/visit-2/src/lib.rs")
            )
            .unwrap(),
            b"fn main() {}"
        );
        assert_eq!(
            std::fs::read(
                output
                    .path()
                    .join("artifacts/nodes/artifact-only/visit-7/logs/output.txt")
            )
            .unwrap(),
            b"hello"
        );
        assert!(!output.path().join("nodes/artifact-only").exists());
    }

    #[test]
    fn inspect_output_dir_rejects_non_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "x").unwrap();

        let err = inspect_output_dir(dir.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("already exists and is not an empty directory")
        );
    }
}
