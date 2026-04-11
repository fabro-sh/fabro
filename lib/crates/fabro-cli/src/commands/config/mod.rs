use std::io::Write;
use std::path::Path;

use fabro_config::effective_settings::{EffectiveSettingsLayers, EffectiveSettingsMode};
use fabro_config::{effective_settings, load_settings_project, project};
use fabro_types::settings::SettingsLayer;

use crate::args::{GlobalArgs, SettingsArgs};
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;
use crate::user_config;

fn config_layers(
    ctx: &CommandContext,
    workflow: Option<&Path>,
) -> anyhow::Result<EffectiveSettingsLayers> {
    let cwd = ctx.cwd();
    let (workflow_layer, project_layer) = match workflow {
        Some(path) => workflow_and_project_layers(path, cwd)?,
        None => (SettingsLayer::default(), load_settings_project(cwd)?),
    };
    let user_layer = user_config::settings_layer_with_config_and_storage_dir(
        Some(ctx.base_config_path()),
        None,
    )?;
    Ok(EffectiveSettingsLayers::new(
        SettingsLayer::default(),
        workflow_layer,
        project_layer,
        user_layer,
    ))
}

fn workflow_and_project_layers(
    path: &Path,
    cwd: &Path,
) -> anyhow::Result<(SettingsLayer, SettingsLayer)> {
    let resolution = project::resolve_workflow_path(path, cwd)?;
    if resolution.workflow_config.is_none() && !resolution.resolved_workflow_path.is_file() {
        anyhow::bail!(
            "Workflow not found: {}",
            resolution.resolved_workflow_path.display()
        );
    }

    let workflow_layer = resolution.workflow_config.unwrap_or_default();
    let project_layer = project::discover_project_config(
        resolution
            .resolved_workflow_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )?
    .map(|(_, config)| config)
    .unwrap_or_default();

    Ok((workflow_layer, project_layer))
}

async fn merged_config(args: &SettingsArgs) -> anyhow::Result<SettingsLayer> {
    let base_ctx = CommandContext::base()?;
    let layers = config_layers(&base_ctx, args.workflow.as_deref())?;
    if args.local {
        return effective_settings::resolve_settings(
            layers,
            None,
            EffectiveSettingsMode::LocalOnly,
        );
    }

    let ctx = CommandContext::for_target(&args.target)?;
    let target = user_config::resolve_server_target(&args.target, ctx.machine_settings())?;
    let server_settings = ctx.server().await?.retrieve_server_settings().await?;
    let mode = match target {
        user_config::ServerTarget::HttpUrl { .. } => EffectiveSettingsMode::RemoteServer,
        user_config::ServerTarget::UnixSocket(_) => EffectiveSettingsMode::LocalDaemon,
    };

    effective_settings::resolve_settings(layers, Some(&server_settings), mode)
}

pub(crate) async fn execute(args: &SettingsArgs, globals: &GlobalArgs) -> anyhow::Result<()> {
    let config = Box::pin(merged_config(args)).await?;
    if globals.json {
        print_json_pretty(&config)?;
        return Ok(());
    }

    let mut yaml = serde_yaml::to_string(&config)?;
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(yaml.as_bytes())?;

    Ok(())
}
