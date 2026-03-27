use std::path::PathBuf;

use crate::args::RunArgs;
use fabro_config::{FabroConfig, FabroSettings};

use fabro_util::terminal::Styles;

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
    let settings: FabroSettings = fabro_workflows::operations::resolve_settings_for_path(
        workflow_path,
        cli_defaults,
        cli_args_config,
        true,
    )?;
    let working_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let base_branch = fabro_sandbox::daytona::detect_repo_info(&working_directory)
        .ok()
        .and_then(|(_, branch)| branch);

    let created =
        match fabro_workflows::operations::create(fabro_workflows::operations::CreateRequest {
            workflow: fabro_workflows::operations::WorkflowInput::Path(workflow_path.clone()),
            settings,
            run_dir: None,
            run_id: args.run_id.clone(),
            base_branch,
            host_repo_path: Some(working_directory.to_string_lossy().to_string()),
        }) {
            Ok(created) => created,
            Err(fabro_workflows::error::FabroError::ValidationFailed { diagnostics }) => {
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
