use std::path::PathBuf;

use crate::args::RunArgs;
use fabro_config::ConfigLayer;
use fabro_types::{RunId, Settings};
use fabro_util::terminal::Styles;
use fabro_workflow::operations::make_run_dir;

use super::output::{api_diagnostics_to_local, print_preflight_workflow_summary};
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest, run_manifest_args};
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
        .clone()
        .combine(ConfigLayer::for_workflow(workflow_path, &cwd)?)
        .combine(cli_defaults)
        .resolve()?;
    let run_id = args
        .run_id
        .as_deref()
        .map(str::parse::<RunId>)
        .transpose()
        .map_err(|err| anyhow::anyhow!("invalid run ID: {err}"))?;

    let built = build_run_manifest(ManifestBuildInput {
        workflow: workflow_path.clone(),
        cwd,
        args_layer: cli_args_config,
        args: run_manifest_args(args),
        run_id,
    })?;

    let client = server_client::connect_server(settings.storage_dir().as_path()).await?;
    if !quiet {
        let preflight = client.run_preflight(built.manifest.clone()).await?;
        let diagnostics = api_diagnostics_to_local(&preflight.workflow.diagnostics);
        if !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
        {
            print_preflight_workflow_summary(&preflight.workflow, Some(&built.target_path), styles);
        }
    }

    let created_run_id = client.create_run_from_manifest(built.manifest).await?;
    let run_dir = make_run_dir(&settings.storage_dir().join("runs"), &created_run_id);

    Ok((created_run_id, run_dir))
}
