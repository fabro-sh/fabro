use std::path::{Path, PathBuf};

use anyhow::Result;
use fabro_types::RunId;
use serde::Serialize;

use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};
use fabro_workflow::run_status::RunStatus;

use crate::args::{GlobalArgs, InspectArgs};
use crate::store;
use crate::user_config::load_user_settings_with_globals;

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

pub(crate) async fn run(args: &InspectArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run).await?;
    let run_store = store::open_run_reader(&cli_settings.storage_dir(), &run.run_id).await?;
    let output = inspect_run_store(&run.run_id, &run.path, run.status, &run_store).await;
    let json = serde_json::to_string_pretty(&[output])?;
    println!("{json}");
    Ok(())
}

async fn inspect_run_store(
    run_id: &RunId,
    run_dir: &Path,
    status: RunStatus,
    run_store: &fabro_store::SlateRunStore,
) -> InspectOutput {
    if let Ok(state) = run_store.state().await {
        return InspectOutput {
            run_id: run_id.to_string(),
            run_dir: run_dir.to_path_buf(),
            status: state.status.as_ref().map_or(status, |record| record.status),
            run_record: state
                .run
                .and_then(|record| serde_json::to_value(record).ok()),
            start_record: state
                .start
                .and_then(|record| serde_json::to_value(record).ok()),
            conclusion: state
                .conclusion
                .and_then(|record| serde_json::to_value(record).ok()),
            checkpoint: state
                .checkpoint
                .and_then(|record| serde_json::to_value(record).ok()),
            sandbox: state
                .sandbox
                .and_then(|record| serde_json::to_value(record).ok()),
        };
    }

    InspectOutput {
        run_id: run_id.to_string(),
        run_dir: run_dir.to_path_buf(),
        status,
        run_record: None,
        start_record: None,
        conclusion: None,
        checkpoint: None,
        sandbox: None,
    }
}
