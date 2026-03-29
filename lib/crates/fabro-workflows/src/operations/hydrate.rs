use std::io::{BufRead, ErrorKind};
use std::path::Path;
use std::sync::Arc;

use fabro_sandbox::{SandboxRecord, SandboxRecordExt};
use fabro_store::{EventPayload, RunStore, Store};
use tracing::warn;

use crate::error::FabroError;
use crate::records::{
    Checkpoint, CheckpointExt, Conclusion, ConclusionExt, RunRecord, RunRecordExt, StartRecord,
    StartRecordExt,
};
use crate::run_status::{RunStatusRecord, RunStatusRecordExt};
use fabro_retro::{RetroExt, retro::Retro};

const GRAPH_FILE_NAME: &str = "workflow.fabro";
const LEGACY_GRAPH_FILE_NAME: &str = "graph.fabro";

pub async fn open_or_hydrate_run(
    store: &dyn Store,
    run_dir: &Path,
) -> Result<Arc<dyn RunStore>, FabroError> {
    let record = RunRecord::load(run_dir)?;
    if let Some(run_store) = store.open_run(&record.run_id).await.map_err(store_error)? {
        return Ok(run_store);
    }

    let run_dir_string = run_dir.to_string_lossy().to_string();
    let run_store = store
        .create_run(&record.run_id, record.created_at, Some(&run_dir_string))
        .await
        .map_err(store_error)?;

    run_store.put_run(&record).await.map_err(store_error)?;

    if let Some(dot_source) = load_graph_source(run_dir)? {
        run_store
            .put_graph(&dot_source)
            .await
            .map_err(store_error)?;
    }

    if let Some(status) = load_status_record(run_dir)? {
        run_store.put_status(&status).await.map_err(store_error)?;
    }
    if let Some(start) = load_start_record(run_dir)? {
        run_store.put_start(&start).await.map_err(store_error)?;
    }
    if let Some(checkpoint) = load_checkpoint(run_dir)? {
        run_store
            .put_checkpoint(&checkpoint)
            .await
            .map_err(store_error)?;
    }
    if let Some(conclusion) = load_conclusion(run_dir)? {
        run_store
            .put_conclusion(&conclusion)
            .await
            .map_err(store_error)?;
    }
    if let Some(retro) = load_retro(run_dir)? {
        run_store.put_retro(&retro).await.map_err(store_error)?;
    }
    if let Some(sandbox) = load_sandbox_record(run_dir)? {
        run_store.put_sandbox(&sandbox).await.map_err(store_error)?;
    }

    hydrate_events(run_dir, &record.run_id, run_store.as_ref()).await?;

    Ok(run_store)
}

async fn hydrate_events(
    run_dir: &Path,
    run_id: &str,
    run_store: &dyn RunStore,
) -> Result<(), FabroError> {
    let progress_path = run_dir.join("progress.jsonl");
    if !progress_path.exists() {
        return Ok(());
    }

    let file = std::fs::File::open(&progress_path)?;
    for (line_number, line_result) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line_result?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => value,
            Err(err) => {
                warn!(
                    path = %progress_path.display(),
                    line_number = line_number + 1,
                    error = %err,
                    "Skipping malformed progress event during hydration"
                );
                continue;
            }
        };

        let payload = match EventPayload::new(value, run_id) {
            Ok(payload) => payload,
            Err(err) => {
                warn!(
                    path = %progress_path.display(),
                    line_number = line_number + 1,
                    error = %err,
                    "Skipping invalid progress event during hydration"
                );
                continue;
            }
        };

        run_store
            .append_event(&payload)
            .await
            .map_err(store_error)?;
    }

    Ok(())
}

fn load_graph_source(run_dir: &Path) -> Result<Option<String>, FabroError> {
    for name in [GRAPH_FILE_NAME, LEGACY_GRAPH_FILE_NAME] {
        match std::fs::read_to_string(run_dir.join(name)) {
            Ok(source) => return Ok(Some(source)),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(None)
}

fn load_status_record(run_dir: &Path) -> Result<Option<RunStatusRecord>, FabroError> {
    let path = run_dir.join("status.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        RunStatusRecord::load(&path).map_err(|err| FabroError::Io(err.to_string()))?,
    ))
}

fn load_start_record(run_dir: &Path) -> Result<Option<StartRecord>, FabroError> {
    let path = run_dir.join("start.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(StartRecord::load(run_dir)?))
}

fn load_checkpoint(run_dir: &Path) -> Result<Option<Checkpoint>, FabroError> {
    let path = run_dir.join("checkpoint.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(Checkpoint::load(&path)?))
}

fn load_conclusion(run_dir: &Path) -> Result<Option<Conclusion>, FabroError> {
    let path = run_dir.join("conclusion.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(Conclusion::load(&path)?))
}

fn load_retro(run_dir: &Path) -> Result<Option<Retro>, FabroError> {
    let path = run_dir.join("retro.json");
    if !path.exists() {
        return Ok(None);
    }
    Retro::load(run_dir)
        .map(Some)
        .map_err(|err| FabroError::Io(err.to_string()))
}

fn load_sandbox_record(run_dir: &Path) -> Result<Option<SandboxRecord>, FabroError> {
    let path = run_dir.join("sandbox.json");
    if !path.exists() {
        return Ok(None);
    }
    SandboxRecord::load(&path)
        .map(Some)
        .map_err(|err| FabroError::Io(err.to_string()))
}

fn store_error(err: impl std::fmt::Display) -> FabroError {
    FabroError::engine(err.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use chrono::Utc;
    use fabro_config::FabroSettings;
    use fabro_graphviz::graph::Graph;
    use fabro_store::{InMemoryStore, Store};
    use fabro_types::{Conclusion, RunStatus, RunStatusRecord, StageStatus};

    use super::open_or_hydrate_run;
    use crate::event::{WorkflowRunEvent, append_progress_event};
    use crate::records::{Checkpoint, CheckpointExt, ConclusionExt, RunRecord, RunRecordExt};
    use crate::run_status::RunStatusRecordExt;

    fn write_run(run_dir: &Path) {
        let record = RunRecord {
            run_id: "run-123".to_string(),
            created_at: Utc::now(),
            settings: FabroSettings::default(),
            graph: Graph::new("test"),
            workflow_slug: Some("test".to_string()),
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path: Some("/tmp/project".to_string()),
            base_branch: Some("main".to_string()),
            labels: HashMap::new(),
        };
        std::fs::create_dir_all(run_dir).unwrap();
        record.save(run_dir).unwrap();
        std::fs::write(
            run_dir.join("workflow.fabro"),
            "digraph test { start -> exit }",
        )
        .unwrap();
        RunStatusRecord::new(RunStatus::Running, None)
            .save(&run_dir.join("status.json"))
            .unwrap();
        let checkpoint = Checkpoint::from_context(
            &crate::context::Context::new(),
            "start",
            vec!["start".to_string()],
            HashMap::new(),
            HashMap::new(),
            Some("exit".to_string()),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        checkpoint.save(&run_dir.join("checkpoint.json")).unwrap();
        let conclusion = Conclusion {
            timestamp: Utc::now(),
            status: StageStatus::Success,
            duration_ms: 5,
            failure_reason: None,
            final_git_commit_sha: None,
            stages: Vec::new(),
            total_cost: None,
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: false,
        };
        conclusion.save(&run_dir.join("conclusion.json")).unwrap();
        append_progress_event(
            run_dir,
            "run-123",
            &WorkflowRunEvent::RunNotice {
                level: crate::event::RunNoticeLevel::Info,
                code: "hydrated".to_string(),
                message: "hello".to_string(),
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn hydrates_run_records_into_store() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run-123");
        write_run(&run_dir);

        let store = InMemoryStore::default();
        let run_store = open_or_hydrate_run(&store, &run_dir).await.unwrap();

        assert_eq!(
            run_store.get_run().await.unwrap().unwrap().run_id,
            "run-123"
        );
        assert!(run_store.get_checkpoint().await.unwrap().is_some());
        assert!(run_store.get_conclusion().await.unwrap().is_some());
        assert_eq!(run_store.list_events().await.unwrap().len(), 1);

        let listed = store
            .list_runs(&fabro_store::ListRunsQuery::default())
            .await
            .unwrap();
        assert_eq!(
            listed[0].run_dir.as_deref(),
            Some(run_dir.to_string_lossy().as_ref())
        );
    }
}
