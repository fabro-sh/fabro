use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
#[cfg(test)]
use fabro_store::NodeVisitRef;
use fabro_store::{RunState, SlateRunStore};
use fabro_workflow::run_dump::RunDump;
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};
#[cfg(test)]
use serde::de::DeserializeOwned;

use crate::args::{GlobalArgs, StoreDumpArgs};
use crate::shared::{absolute_or_current, print_json_pretty};
use crate::store;
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn dump_command(args: &StoreDumpArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run).await?;
    let run_store = store::open_run_reader(&cli_settings.storage_dir(), &run.run_id).await?;

    let file_count = export_run(&run_store, &args.output).await?;
    if globals.json {
        print_json_pretty(&serde_json::json!({
            "run_id": run.run_id,
            "output_dir": absolute_or_current(&args.output),
            "file_count": file_count,
        }))?;
    } else {
        println!(
            "Exported {file_count} files for run {} to {}",
            run.run_id,
            args.output.display()
        );
    }
    Ok(())
}

pub(crate) async fn export_run(run_store: &SlateRunStore, output_dir: &Path) -> Result<usize> {
    let state = run_store.state().await?;
    anyhow::ensure!(state.run.is_some(), "run has no data in the store");

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

    let file_count = export_run_to_dir(run_store, &state, &staging_path).await?;

    if matches!(output_state, OutputDirState::ExistingEmpty) {
        std::fs::remove_dir(output_dir)
            .with_context(|| format!("failed to replace {}", output_dir.display()))?;
    }
    std::fs::rename(&staging_path, output_dir).with_context(|| {
        format!(
            "failed to move staged export {} into {}",
            staging_path.display(),
            output_dir.display()
        )
    })?;
    let _ = staging_dir.keep();

    Ok(file_count)
}

async fn export_run_to_dir(
    run_store: &SlateRunStore,
    state: &RunState,
    output_dir: &Path,
) -> Result<usize> {
    let dump = RunDump::store_export(run_store, state).await?;
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
    use fabro_store::{EventEnvelope, EventPayload, SlateStore};
    use fabro_types::{
        AggregateStats, AttrValue, Checkpoint, Conclusion, Graph, NodeStatusRecord, Retro, RunId,
        RunRecord, RunStatus, RunStatusRecord, SandboxRecord, Settings, StageStatus, StartRecord,
        StatusReason, fixtures,
    };
    use fabro_workflow::event::{WorkflowRunEvent, append_workflow_event};
    use object_store::memory::InMemory;

    fn dt(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn test_store() -> Arc<SlateStore> {
        Arc::new(SlateStore::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    fn sample_run_record(run_id: RunId, _created_at: DateTime<Utc>) -> RunRecord {
        let mut graph = Graph::new("night-sky");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("map the constellations".to_string()),
        );
        RunRecord {
            run_id,
            settings: Settings::default(),
            graph,
            workflow_slug: Some("night-sky".to_string()),
            working_directory: PathBuf::from("/tmp/night-sky"),
            host_repo_path: Some("github.com/fabro-sh/fabro".to_string()),
            base_branch: Some("main".to_string()),
            labels: HashMap::from([("team".to_string(), "infra".to_string())]),
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

    fn read_json<T: DeserializeOwned>(path: &Path) -> T {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()))
    }

    #[tokio::test]
    async fn export_run_writes_expected_directory_tree() {
        let store = test_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run_id = test_run_id();
        let run = store.create_run(&run_id).await.unwrap();
        let run_record = sample_run_record(run_id, created_at);
        let start_record = sample_start_record(run_id, created_at);
        let status_record = sample_status();
        let first_checkpoint = sample_checkpoint("plan", 1);
        let second_checkpoint = sample_checkpoint("code", 2);
        let conclusion = sample_conclusion();
        let retro = sample_retro(run_id);
        let sandbox = sample_sandbox();

        let node = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::RunCreated {
                run_id,
                settings: serde_json::to_value(&run_record.settings).unwrap(),
                graph: serde_json::to_value(&run_record.graph).unwrap(),
                workflow_source: Some("digraph night_sky {}".to_string()),
                workflow_config: None,
                labels: run_record.labels.clone().into_iter().collect(),
                run_dir: "/tmp/night-sky-run".to_string(),
                working_directory: run_record.working_directory.display().to_string(),
                host_repo_path: run_record.host_repo_path.clone(),
                base_branch: run_record.base_branch.clone(),
                workflow_slug: run_record.workflow_slug.clone(),
                db_prefix: None,
            },
        )
        .await
        .unwrap();
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::WorkflowRunStarted {
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
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::RunRunning {
                reason: status_record.reason,
            },
        )
        .await
        .unwrap();
        for checkpoint in [&first_checkpoint, &second_checkpoint] {
            append_workflow_event(
                &run,
                &run_id,
                &WorkflowRunEvent::CheckpointCompleted {
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
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::SandboxInitialized {
                working_directory: sandbox.working_directory.clone(),
                provider: sandbox.provider.clone(),
                identifier: sandbox.identifier.clone(),
                host_working_directory: sandbox.host_working_directory.clone(),
                container_mount_point: sandbox.container_mount_point.clone(),
            },
        )
        .await
        .unwrap();
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::Prompt {
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
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::PromptCompleted {
                node_id: "code".to_string(),
                response: "Implemented".to_string(),
                model: "gpt-5".to_string(),
                provider: "openai".to_string(),
                usage: None,
            },
        )
        .await
        .unwrap();
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::StageCompleted {
                node_id: "code".to_string(),
                name: "Code".to_string(),
                index: 1,
                duration_ms: 250,
                status: "partial_success".to_string(),
                preferred_label: None,
                suggested_next_ids: Vec::new(),
                usage: None,
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
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::CommandStarted {
                node_id: "code".to_string(),
                script: "echo hi".to_string(),
                language: "sh".to_string(),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::CommandCompleted {
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
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::RetroStarted {
                prompt: Some("How did it go?".to_string()),
                provider: None,
                model: None,
            },
        )
        .await
        .unwrap();
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::RetroCompleted {
                duration_ms: 50,
                response: Some("Smooth enough".to_string()),
                retro: Some(serde_json::to_value(&retro).unwrap()),
            },
        )
        .await
        .unwrap();
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::WorkflowRunCompleted {
                duration_ms: conclusion.duration_ms,
                artifact_count: 0,
                status: "success".to_string(),
                reason: None,
                total_cost: conclusion.total_cost,
                final_git_commit_sha: conclusion.final_git_commit_sha.clone(),
                final_patch: None,
                usage: None,
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
                    "event": "stage.completed"
                }),
                &run_id,
            )
            .unwrap(),
        )
        .await
        .unwrap();
        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
            .await
            .unwrap();
        run.put_artifact_value("plan", &serde_json::json!({"steps": 3}))
            .await
            .unwrap();
        run.put_asset(&node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let asset_only_node = NodeVisitRef {
            node_id: "artifact-only",
            visit: 7,
        };
        run.put_asset(&asset_only_node, "logs/output.txt", b"hello")
            .await
            .unwrap();

        let output = tempfile::tempdir().unwrap();
        let file_count = export_run(&run, output.path()).await.unwrap();
        assert_eq!(file_count, 22);

        let exported_run: RunRecord = read_json(&output.path().join("run.json"));
        assert_eq!(exported_run.run_id, run_id);

        let exported_start: StartRecord = read_json(&output.path().join("start.json"));
        assert_eq!(exported_start.run_id, run_id);

        let exported_status: RunStatusRecord = read_json(&output.path().join("status.json"));
        assert_eq!(exported_status.status, RunStatus::Succeeded);

        let exported_checkpoint: Checkpoint = read_json(&output.path().join("checkpoint.json"));
        assert_eq!(exported_checkpoint.current_node, "code");
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
        assert_eq!(node_status.status, StageStatus::PartialSuccess);
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

        let exported_plan: serde_json::Value =
            read_json(&output.path().join("artifacts/values/plan.json"));
        let exported_summary: serde_json::Value =
            read_json(&output.path().join("artifacts/values/summary.json"));
        assert_eq!(exported_plan, serde_json::json!({"steps": 3}));
        assert_eq!(exported_summary, serde_json::json!({"done": true}));

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

    #[tokio::test]
    async fn export_run_rejects_path_traversal_and_leaves_no_partial_output() {
        let store = test_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run_id = test_run_id();
        let run = store.create_run(&run_id).await.unwrap();
        let run_record = sample_run_record(run_id, created_at);
        append_workflow_event(
            &run,
            &run_id,
            &WorkflowRunEvent::RunCreated {
                run_id,
                settings: serde_json::to_value(&run_record.settings).unwrap(),
                graph: serde_json::to_value(&run_record.graph).unwrap(),
                workflow_source: Some("digraph night_sky {}".to_string()),
                workflow_config: None,
                labels: run_record.labels.clone().into_iter().collect(),
                run_dir: "/tmp/night-sky-run".to_string(),
                working_directory: run_record.working_directory.display().to_string(),
                host_repo_path: run_record.host_repo_path.clone(),
                base_branch: run_record.base_branch.clone(),
                workflow_slug: run_record.workflow_slug.clone(),
                db_prefix: None,
            },
        )
        .await
        .unwrap();
        run.put_asset(
            &NodeVisitRef {
                node_id: "code",
                visit: 1,
            },
            "../escape.txt",
            b"boom",
        )
        .await
        .unwrap();

        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("dump");
        let err = export_run(&run, &output).await.unwrap_err();
        assert!(err.to_string().contains("asset filename"));
        assert!(!output.exists());
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
