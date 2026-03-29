use std::path::{Path, PathBuf};

use anyhow::Result;
use fabro_config::FabroSettingsExt;
use fabro_sandbox::SandboxRecordExt;
use fabro_workflows::records::{CheckpointExt, ConclusionExt, RunRecordExt, StartRecordExt};
use serde::Serialize;

use fabro_workflows::records::{Checkpoint, Conclusion, RunRecord, StartRecord};
use fabro_workflows::run_lookup::{resolve_run_combined, runs_base};
use fabro_workflows::run_status::RunStatus;

use crate::args::InspectArgs;
use crate::cli_config::load_cli_settings;

#[derive(Debug, Serialize)]
pub(crate) struct InspectOutput {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub status: RunStatus,
    pub run_record: Option<serde_json::Value>,
    pub start_record: Option<serde_json::Value>,
    pub conclusion: Option<serde_json::Value>,
    pub checkpoint: Option<serde_json::Value>,
    pub sandbox: Option<serde_json::Value>,
}

pub(crate) async fn run(args: &InspectArgs) -> Result<()> {
    let cli_settings = load_cli_settings()?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = crate::store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run).await?;
    let output =
        match crate::store::open_run_reader(&cli_settings.storage_dir(), &run.run_id).await? {
            Some(run_store) => {
                inspect_run_store(&run.run_id, &run.path, run.status, run_store.as_ref()).await
            }
            None => inspect_run_dir(&run.run_id, &run.path, run.status),
        };
    let json = serde_json::to_string_pretty(&[output])?;
    println!("{json}");
    Ok(())
}

async fn inspect_run_store(
    run_id: &str,
    run_dir: &Path,
    status: RunStatus,
    run_store: &dyn fabro_store::RunStore,
) -> InspectOutput {
    match run_store.get_snapshot().await {
        Ok(Some(snapshot)) => InspectOutput {
            run_id: run_id.to_string(),
            run_dir: run_dir.to_path_buf(),
            status: snapshot
                .status
                .as_ref()
                .map(|record| record.status)
                .unwrap_or(status),
            run_record: serde_json::to_value(snapshot.run).ok(),
            start_record: snapshot
                .start
                .and_then(|record| serde_json::to_value(record).ok()),
            conclusion: snapshot
                .conclusion
                .and_then(|record| serde_json::to_value(record).ok()),
            checkpoint: snapshot
                .checkpoint
                .and_then(|record| serde_json::to_value(record).ok()),
            sandbox: snapshot
                .sandbox
                .and_then(|record| serde_json::to_value(record).ok()),
        },
        _ => inspect_run_dir(run_id, run_dir, status),
    }
}

fn inspect_run_dir(run_id: &str, run_dir: &Path, status: RunStatus) -> InspectOutput {
    let run_record = RunRecord::load(run_dir)
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let start_record = StartRecord::load(run_dir)
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let conclusion = Conclusion::load(&run_dir.join("conclusion.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let checkpoint = Checkpoint::load(&run_dir.join("checkpoint.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let sandbox = fabro_sandbox::SandboxRecord::load(&run_dir.join("sandbox.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());

    InspectOutput {
        run_id: run_id.to_string(),
        run_dir: run_dir.to_path_buf(),
        status,
        run_record,
        start_record,
        conclusion,
        checkpoint,
        sandbox,
    }
}
