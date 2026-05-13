use anyhow::{Context as _, bail};
use fabro_config::RunLayer;
use fabro_config::user::active_settings_path;
use fabro_manifest::{ManifestBuildInput, build_run_manifest};
use fabro_server::manifest_validation;
use fabro_types::RunId;
use fabro_util::terminal::Styles;
use fabro_workflow::operations::RenderMode;

use super::output::{api_diagnostics_to_local, print_workflow_summary};
use super::overrides::run_args_overrides;
use crate::args::RunArgs;
use crate::command_context::CommandContext;
use crate::manifest_args::run_manifest_args;

pub(crate) struct CreatedRun {
    pub(crate) run_id: RunId,
}

/// Create a workflow run: allocate run directory, persist RunSpec, return
/// (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub(crate) async fn create_run(
    ctx: &CommandContext,
    args: &RunArgs,
    styles: &Styles,
    quiet: bool,
) -> anyhow::Result<CreatedRun> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;
    let cli_args_config = run_args_overrides(args)?;
    let cwd = ctx.cwd().to_path_buf();
    let run_id = args
        .run_id
        .as_deref()
        .map(str::parse::<RunId>)
        .transpose()
        .context("invalid run ID")?;

    let built = build_run_manifest(ManifestBuildInput {
        workflow: workflow_path.clone(),
        cwd,
        run_overrides: cli_args_config.run,
        cli_overrides: cli_args_config.cli,
        input_overrides: cli_args_config.input_overrides,
        args: run_manifest_args(args),
        run_id,
        user_settings_path: Some(active_settings_path(None)),
    })?;
    let validation = manifest_validation::validate_manifest(
        &RunLayer::default(),
        &built.manifest,
        RenderMode::Strict,
        ctx.catalog()?,
    )?;
    let diagnostics = api_diagnostics_to_local(&validation.workflow.diagnostics);
    if !quiet {
        print_workflow_summary(
            &validation.workflow,
            Some(&built.target_path),
            styles,
            ctx.printer(),
        );
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
    {
        bail!("Validation failed");
    }

    let client = ctx.server().await?;
    let created_run_id = client
        .create_run_from_manifest(built.manifest)
        .await
        .context("could not create run")?;

    Ok(CreatedRun {
        run_id: created_run_id,
    })
}
