use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::args::InspectArgs;

#[derive(Debug, Serialize)]
pub struct InspectOutput {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub status: fabro_workflows::run_status::RunStatus,
    pub run_record: Option<serde_json::Value>,
    pub start_record: Option<serde_json::Value>,
    pub conclusion: Option<serde_json::Value>,
    pub checkpoint: Option<serde_json::Value>,
    pub sandbox: Option<serde_json::Value>,
}

pub fn run(args: &InspectArgs) -> Result<()> {
    let cli_config = crate::cli_config::load_cli_config(None)?;
    let base = fabro_workflows::run_lookup::runs_base(&cli_config.storage_dir());
    let run = fabro_workflows::run_lookup::resolve_run(&base, &args.run)?;
    let output = inspect_run_dir(&run.run_id, &run.path, run.status)?;
    let json = serde_json::to_string_pretty(&[output])?;
    println!("{json}");
    Ok(())
}

fn inspect_run_dir(
    run_id: &str,
    run_dir: &Path,
    status: fabro_workflows::run_status::RunStatus,
) -> Result<InspectOutput> {
    let run_record = fabro_workflows::records::RunRecord::load(run_dir)
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let start_record = fabro_workflows::records::StartRecord::load(run_dir)
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let conclusion = fabro_workflows::records::Conclusion::load(&run_dir.join("conclusion.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let checkpoint = fabro_workflows::records::Checkpoint::load(&run_dir.join("checkpoint.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let sandbox = fabro_sandbox::SandboxRecord::load(&run_dir.join("sandbox.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());

    Ok(InspectOutput {
        run_id: run_id.to_string(),
        run_dir: run_dir.to_path_buf(),
        status,
        run_record,
        start_record,
        conclusion,
        checkpoint,
        sandbox,
    })
}
