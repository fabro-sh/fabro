use std::path::PathBuf;

use crate::args::RunArgs;
use fabro_config::ConfigLayer;
use fabro_types::{RunId, Settings};
use fabro_util::terminal::Styles;
use fabro_workflow::error::FabroError;
use fabro_workflow::operations::{CreateRunInput, WorkflowInput, create};

use super::output::{print_diagnostics_from_error, print_workflow_report_from_persisted};
use crate::store;

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

    let store = store::build_store(settings.storage_dir().as_path())?;

    let created = match Box::pin(create(
        store.as_ref(),
        CreateRunInput {
            workflow: WorkflowInput::Path(workflow_path.clone()),
            settings,
            cwd,
            workflow_slug: None,
            run_id,
            base_branch: None,
            host_repo_path: None,
        },
    ))
    .await
    {
        Ok(created) => created,
        Err(FabroError::ValidationFailed { diagnostics }) => {
            if !quiet {
                print_diagnostics_from_error(&diagnostics, styles);
            }
            anyhow::bail!("Validation failed");
        }
        Err(err) => return Err(err.into()),
    };

    if !quiet {
        print_workflow_report_from_persisted(
            &created.persisted,
            created.dot_path.as_deref(),
            styles,
        );
    }

    Ok((created.run_id, created.run_dir))
}
