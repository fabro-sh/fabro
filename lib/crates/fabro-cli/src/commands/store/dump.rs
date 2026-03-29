use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use fabro_config::FabroSettingsExt;
use fabro_store::{NodeVisitRef, RunSnapshot, RunStore};
use fabro_workflows::run_lookup::{resolve_run_combined, runs_base};
use serde::Serialize;

use crate::args::StoreDumpArgs;
use crate::cli_config::load_cli_settings;
use crate::store;

pub(crate) async fn dump_command(args: &StoreDumpArgs) -> Result<()> {
    let cli_settings = load_cli_settings(None)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run).await?;
    let run_store = store::open_run_reader(&cli_settings.storage_dir(), &run.run_id)
        .await?
        .with_context(|| {
            format!(
                "run {} is not in the store (it may be a legacy filesystem-only run)",
                run.run_id
            )
        })?;

    let file_count = export_run(run_store.as_ref(), &args.output).await?;
    println!(
        "Exported {file_count} files for run {} to {}",
        run.run_id,
        args.output.display()
    );
    Ok(())
}

pub(crate) async fn export_run(run_store: &dyn RunStore, output_dir: &Path) -> Result<usize> {
    let snapshot = run_store
        .get_snapshot()
        .await?
        .context("run has no data in the store")?;

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

    let file_count = export_run_to_dir(run_store, &snapshot, &staging_path).await?;

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
    run_store: &dyn RunStore,
    snapshot: &RunSnapshot,
    output_dir: &Path,
) -> Result<usize> {
    let mut file_count = 0;

    write_json_file(&output_dir.join("run.json"), &snapshot.run)?;
    file_count += 1;
    file_count += usize::from(write_optional_json_file(
        &output_dir.join("start.json"),
        snapshot.start.as_ref(),
    )?);
    file_count += usize::from(write_optional_json_file(
        &output_dir.join("status.json"),
        snapshot.status.as_ref(),
    )?);
    file_count += usize::from(write_optional_json_file(
        &output_dir.join("checkpoint.json"),
        snapshot.checkpoint.as_ref(),
    )?);
    file_count += usize::from(write_optional_json_file(
        &output_dir.join("conclusion.json"),
        snapshot.conclusion.as_ref(),
    )?);
    file_count += usize::from(write_optional_json_file(
        &output_dir.join("retro.json"),
        snapshot.retro.as_ref(),
    )?);
    file_count += usize::from(write_optional_text_file(
        &output_dir.join("graph.fabro"),
        snapshot.graph.as_deref(),
    )?);
    file_count += usize::from(write_optional_json_file(
        &output_dir.join("sandbox.json"),
        snapshot.sandbox.as_ref(),
    )?);

    for node in &snapshot.nodes {
        let node_id = validate_single_path_segment("node id", &node.node_id)?;
        let base = output_dir
            .join("nodes")
            .join(node_id)
            .join(format!("visit-{}", node.visit));
        file_count += usize::from(write_optional_text_file(
            &base.join("prompt.md"),
            node.prompt.as_deref(),
        )?);
        file_count += usize::from(write_optional_text_file(
            &base.join("response.md"),
            node.response.as_deref(),
        )?);
        file_count += usize::from(write_optional_json_file(
            &base.join("status.json"),
            node.status.as_ref(),
        )?);
        file_count += usize::from(write_optional_text_file(
            &base.join("stdout.log"),
            node.stdout.as_deref(),
        )?);
        file_count += usize::from(write_optional_text_file(
            &base.join("stderr.log"),
            node.stderr.as_deref(),
        )?);
    }

    file_count += usize::from(write_optional_text_file(
        &output_dir.join("retro").join("prompt.md"),
        run_store.get_retro_prompt().await?.as_deref(),
    )?);
    file_count += usize::from(write_optional_text_file(
        &output_dir.join("retro").join("response.md"),
        run_store.get_retro_response().await?.as_deref(),
    )?);

    write_events_jsonl(
        &output_dir.join("events.jsonl"),
        &run_store.list_events().await?,
    )?;
    file_count += 1;

    for (seq, checkpoint) in run_store.list_checkpoints().await? {
        write_json_file(
            &output_dir
                .join("checkpoints")
                .join(format!("{seq:04}.json")),
            &checkpoint,
        )?;
        file_count += 1;
    }

    for artifact_id in run_store.list_artifact_values().await? {
        let artifact_id_segment = validate_single_path_segment("artifact id", &artifact_id)?;
        let value = run_store
            .get_artifact_value(&artifact_id)
            .await?
            .with_context(|| format!("artifact value {artifact_id:?} is missing from the store"))?;
        write_json_file(
            &output_dir
                .join("artifacts")
                .join("values")
                .join(format!("{}.json", artifact_id_segment.display())),
            &value,
        )?;
        file_count += 1;
    }

    for (node_id, visit, filename) in run_store.list_all_assets().await? {
        let node_id_segment = validate_single_path_segment("node id", &node_id)?;
        let filename_path = validate_relative_path("asset filename", &filename)?;
        let node = NodeVisitRef {
            node_id: &node_id,
            visit,
        };
        let data = run_store
            .get_asset(&node, &filename)
            .await?
            .with_context(|| {
                format!(
                    "asset {filename:?} for node {node_id:?} visit {visit} is missing from the store"
                )
            })?;
        write_bytes_file(
            &output_dir
                .join("artifacts")
                .join("nodes")
                .join(node_id_segment)
                .join(format!("visit-{visit}"))
                .join(filename_path),
            data.as_ref(),
        )?;
        file_count += 1;
    }

    Ok(file_count)
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

fn write_optional_json_file<T>(path: &Path, value: Option<&T>) -> Result<bool>
where
    T: Serialize,
{
    match value {
        Some(value) => {
            write_json_file(path, value)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn write_json_file<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    ensure_parent_dir(path)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    std::fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn write_optional_text_file(path: &Path, value: Option<&str>) -> Result<bool> {
    match value {
        Some(value) => {
            write_text_file(path, value)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn write_text_file(path: &Path, value: &str) -> Result<()> {
    write_bytes_file(path, value.as_bytes())
}

fn write_bytes_file(path: &Path, value: &[u8]) -> Result<()> {
    ensure_parent_dir(path)?;
    std::fs::write(path, value).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn write_events_jsonl(path: &Path, events: &[fabro_store::EventEnvelope]) -> Result<()> {
    ensure_parent_dir(path)?;
    let mut file = std::fs::File::create(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    for event in events {
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
    }
    Ok(())
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
    use super::*;

    use std::collections::HashMap;
    use std::path::PathBuf;

    use chrono::{DateTime, Utc};
    use fabro_store::{EventEnvelope, EventPayload, InMemoryStore, Store as _};
    use fabro_types::{
        AggregateStats, AttrValue, Checkpoint, Conclusion, FabroSettings, Graph, NodeStatusRecord,
        Retro, RunRecord, RunStatus, RunStatusRecord, SandboxRecord, StageStatus, StartRecord,
        StatusReason,
    };

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
            start_time: created_at + chrono::Duration::seconds(5),
            run_branch: Some("fabro/run/demo".to_string()),
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

    fn sample_retro(run_id: &str) -> Retro {
        Retro {
            run_id: run_id.to_string(),
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

    fn event_payload(run_id: &str, ts: &str, event: &str) -> EventPayload {
        EventPayload::new(
            serde_json::json!({
                "ts": ts,
                "run_id": run_id,
                "event": event
            }),
            run_id,
        )
        .unwrap()
    }

    fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn export_run_writes_expected_directory_tree() {
        let store = InMemoryStore::default();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store.create_run("run-1", created_at, None).await.unwrap();

        run.put_run(&sample_run_record("run-1", created_at))
            .await
            .unwrap();
        run.put_start(&sample_start_record("run-1", created_at))
            .await
            .unwrap();
        run.put_status(&sample_status()).await.unwrap();
        run.append_checkpoint(&sample_checkpoint("plan", 1))
            .await
            .unwrap();
        run.append_checkpoint(&sample_checkpoint("code", 2))
            .await
            .unwrap();
        run.put_conclusion(&sample_conclusion()).await.unwrap();
        run.put_retro(&sample_retro("run-1")).await.unwrap();
        run.put_graph("digraph night_sky {}").await.unwrap();
        run.put_sandbox(&sample_sandbox()).await.unwrap();

        let node = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        run.put_node_prompt(&node, "Plan the fix").await.unwrap();
        run.put_node_response(&node, "Implemented").await.unwrap();
        run.put_node_status(&node, &sample_node_status())
            .await
            .unwrap();
        run.put_node_stdout(&node, "stdout line").await.unwrap();
        run.put_node_stderr(&node, "").await.unwrap();
        run.put_retro_prompt("How did it go?").await.unwrap();
        run.put_retro_response("Smooth enough").await.unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00.000Z",
            "WorkflowRunStarted",
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:01.000Z",
            "StageCompleted",
        ))
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
        let file_count = export_run(run.as_ref(), output.path()).await.unwrap();
        assert_eq!(file_count, 22);

        let exported_run: RunRecord = read_json(&output.path().join("run.json"));
        assert_eq!(exported_run.run_id, "run-1");

        let exported_start: StartRecord = read_json(&output.path().join("start.json"));
        assert_eq!(exported_start.run_id, "run-1");

        let exported_status: RunStatusRecord = read_json(&output.path().join("status.json"));
        assert_eq!(exported_status.status, RunStatus::Running);

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
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);

        let first_checkpoint: Checkpoint = read_json(&output.path().join("checkpoints/0001.json"));
        let second_checkpoint: Checkpoint = read_json(&output.path().join("checkpoints/0002.json"));
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
        let store = InMemoryStore::default();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store.create_run("run-1", created_at, None).await.unwrap();

        run.put_run(&sample_run_record("run-1", created_at))
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
        let err = export_run(run.as_ref(), &output).await.unwrap_err();
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
