use std::path::{Path, PathBuf};

use anyhow::Result;
use fabro_types::RunId;
use serde::Serialize;

use fabro_workflow::run_lookup::{resolve_run_from_summaries, runs_base};
use fabro_workflow::run_status::RunStatus;

use crate::args::{GlobalArgs, InspectArgs};
use crate::server_client;
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
    let client = server_client::connect_server(&cli_settings.storage_dir()).await?;
    let summaries = client.list_store_runs().await?;
    let run = resolve_run_from_summaries(&summaries, &base, &args.run)?;
    let run_id = run.run_id();
    let state = client.get_run_state(&run_id).await?;
    let output = inspect_run_state(&run_id, &run.path, run.status(), state);
    let json = serde_json::to_string_pretty(&[output])?;
    println!("{json}");
    Ok(())
}

fn inspect_run_state(
    run_id: &RunId,
    run_dir: &Path,
    status: RunStatus,
    state: crate::server_client::RunProjection,
) -> InspectOutput {
    InspectOutput {
        run_id: run_id.to_string(),
        run_dir: run_dir.to_path_buf(),
        status: state.status.as_ref().map_or(status, |record| record.status),
        run_record: state.run.and_then(|record| serde_json::to_value(record).ok()),
        start_record: state.start.and_then(|record| serde_json::to_value(record).ok()),
        conclusion: state
            .conclusion
            .and_then(|record| serde_json::to_value(record).ok()),
        checkpoint: state
            .checkpoint
            .and_then(|record| serde_json::to_value(record).ok()),
        sandbox: state
            .sandbox
            .and_then(|record| serde_json::to_value(record).ok()),
    }
}
