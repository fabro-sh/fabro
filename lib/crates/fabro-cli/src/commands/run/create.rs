use std::path::PathBuf;

use crate::args::RunArgs;
use fabro_config::ConfigLayer;
use fabro_types::{RunId, Settings};
use fabro_util::terminal::Styles;
use fabro_workflow::operations::make_run_dir;

use super::output::{api_diagnostics_to_local, print_preflight_workflow_summary};
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest, run_manifest_args};
use crate::server_client;
use crate::user_config::{self, ServerConnection};

pub(crate) struct CreatedRun {
    pub(crate) run_id: RunId,
    pub(crate) local_run_dir: Option<PathBuf>,
    pub(crate) connection: ServerConnection,
}

/// Create a workflow run: allocate run directory, persist RunRecord, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub(crate) async fn create_run(
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
    let connection = user_config::server_backed_command_connection(&args.target, &settings)?;
    let client = server_client::connect_server_connection(&connection).await?;
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
    let local_run_dir = match &connection {
        ServerConnection::Local { storage_dir } => {
            Some(make_run_dir(&storage_dir.join("runs"), &created_run_id))
        }
        ServerConnection::Target(_) => None,
    };

    Ok(CreatedRun {
        run_id: created_run_id,
        local_run_dir,
        connection,
    })
}
