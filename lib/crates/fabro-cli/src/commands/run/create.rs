use std::path::PathBuf;

use crate::args::RunArgs;
use fabro_config::ConfigLayer;
use fabro_types::{RunId, Settings};
use fabro_util::terminal::Styles;
use fabro_workflow::operations::{ValidateInput, WorkflowInput, make_run_dir, validate};

use super::output::print_workflow_report;
use crate::server_client;

/// Create a workflow run: allocate run directory, persist RunRecord, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub(crate) async fn create_run(
    args: &RunArgs,
    cli_defaults: ConfigLayer,
    styles: &Styles,
    quiet: bool,
) -> anyhow::Result<(RunId, PathBuf)> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;
    let cli_args_config = ConfigLayer::try_from(args)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let settings: Settings = cli_args_config
        .combine(ConfigLayer::for_workflow(workflow_path, &cwd)?)
        .combine(cli_defaults)
        .resolve()?;

    let run_id = args
        .run_id
        .as_deref()
        .map(str::parse::<RunId>)
        .transpose()
        .map_err(|err| anyhow::anyhow!("invalid run ID: {err}"))?;

    if !quiet {
        let validated = validate(ValidateInput {
            workflow: WorkflowInput::Path(workflow_path.clone()),
            settings: settings.clone(),
            cwd: cwd.clone(),
            custom_transforms: Vec::new(),
        });
        if let Ok(validated) = validated {
            if !validated.has_errors() {
                print_workflow_report(&validated, Some(workflow_path.as_path()), styles);
            }
        }
    }

    let client = server_client::connect_server(settings.storage_dir().as_path()).await?;
    let created_run_id = client
        .create_run_from_workflow_path(workflow_path, &cwd, &settings, run_id.as_ref())
        .await?;
    let run_dir = make_run_dir(&settings.storage_dir().join("runs"), &created_run_id);

    Ok((created_run_id, run_dir))
}
