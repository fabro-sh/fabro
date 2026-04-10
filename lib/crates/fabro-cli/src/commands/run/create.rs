use std::path::PathBuf;

use crate::args::RunArgs;
use crate::command_context::CommandContext;
use fabro_config::ConfigLayer;
use fabro_config::Storage;
use fabro_types::RunId;
use fabro_types::settings::SettingsFile;
use fabro_util::terminal::Styles;

use super::output::{api_diagnostics_to_local, print_preflight_workflow_summary};
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest, run_manifest_args};
use crate::user_config::{self, ServerTarget};

pub(crate) struct CreatedRun {
    pub(crate) run_id: RunId,
    pub(crate) local_run_dir: Option<PathBuf>,
}

/// Create a workflow run: allocate run directory, persist RunRecord, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub(crate) async fn create_run(
    ctx: &CommandContext,
    args: &RunArgs,
    cli_defaults: ConfigLayer,
    styles: &Styles,
    quiet: bool,
) -> anyhow::Result<CreatedRun> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;
    let cli_args_config = ConfigLayer::try_from(args)?;
    let cwd = ctx.cwd().to_path_buf();
    let _settings: SettingsFile = cli_args_config
        .clone()
        .combine(ConfigLayer::for_workflow(workflow_path, &cwd)?)
        .combine(cli_defaults)
        .into();
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
    let target = user_config::resolve_server_target(&args.target, ctx.machine_settings())?;
    let client = ctx.server().await?;
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
    let local_run_dir = match &target {
        ServerTarget::UnixSocket(_) => Some(
            Storage::new(ctx.machine_settings().storage_dir())
                .run_scratch(&created_run_id)
                .root()
                .to_path_buf(),
        ),
        ServerTarget::HttpUrl { .. } => None,
    };

    Ok(CreatedRun {
        run_id: created_run_id,
        local_run_dir,
    })
}
