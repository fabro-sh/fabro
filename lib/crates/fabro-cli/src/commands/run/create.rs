use std::path::PathBuf;

use fabro_config::Storage;
use fabro_config::load::load_settings_user;
use fabro_config::user::active_settings_path;
use fabro_types::RunId;
use fabro_types::settings::SettingsLayer;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use super::output::{api_diagnostics_to_local, print_preflight_workflow_summary};
use super::overrides::run_args_layer;
use crate::args::RunArgs;
use crate::command_context::CommandContext;
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest, run_manifest_args};
use crate::user_config::{self, ServerTarget};

pub(crate) struct CreatedRun {
    pub(crate) run_id:        RunId,
    pub(crate) local_run_dir: Option<PathBuf>,
}

/// Create a workflow run: allocate run directory, persist RunRecord, return
/// (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub(crate) async fn create_run(
    ctx: &CommandContext,
    args: &RunArgs,
    _cli_defaults: SettingsLayer,
    styles: &Styles,
    quiet: bool,
    printer: Printer,
) -> anyhow::Result<CreatedRun> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;
    let cli_args_config = run_args_layer(args)?;
    let cwd = ctx.cwd().to_path_buf();
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
        user_layer: load_settings_user()?,
        user_settings_path: Some(active_settings_path(None)),
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
            print_preflight_workflow_summary(
                &preflight.workflow,
                Some(&built.target_path),
                styles,
                printer,
            );
        }
    }

    let created_run_id = client.create_run_from_manifest(built.manifest).await?;
    let local_run_dir = match &target {
        ServerTarget::UnixSocket(_) => Some(
            Storage::new(user_config::storage_dir(ctx.machine_settings())?)
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
