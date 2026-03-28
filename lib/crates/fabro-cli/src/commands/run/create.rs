use std::path::PathBuf;

use crate::args::RunArgs;
use fabro_config::project::{ResolveSettingsInput, resolve_settings};
use fabro_config::{FabroConfig, FabroSettings};
use fabro_util::terminal::Styles;
use fabro_workflows::error::FabroError;
use fabro_workflows::operations::{CreateRunInput, WorkflowInput, create};

use super::output::{print_diagnostics_from_error, print_workflow_report_from_persisted};

/// Create a workflow run: allocate run directory, persist RunRecord, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub async fn create_run(
    args: &RunArgs,
    cli_defaults: FabroConfig,
    styles: &Styles,
    quiet: bool,
) -> anyhow::Result<(String, PathBuf)> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;
    let cli_args_config = FabroConfig::try_from(args)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let settings: FabroSettings = resolve_settings(ResolveSettingsInput {
        workflow_path: workflow_path.clone(),
        cwd: cwd.clone(),
        defaults: cli_defaults,
        overrides: cli_args_config,
        apply_project_config: true,
    })?;

    let created = match create(CreateRunInput {
        workflow: WorkflowInput::Path(workflow_path.clone()),
        settings,
        cwd,
        workflow_slug: None,
        run_dir: None,
        run_id: args.run_id.clone(),
        base_branch: None,
        host_repo_path: None,
    }) {
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
