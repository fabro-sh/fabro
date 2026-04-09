use std::io::Write;
use std::path::Path;

use crate::args::{GlobalArgs, SettingsArgs};
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;
use crate::user_config;
use fabro_config::ConfigLayer;
use fabro_config::effective_settings;
use fabro_config::effective_settings::{EffectiveSettingsLayers, EffectiveSettingsMode};
use fabro_config::project;
use fabro_types::settings::v2::SettingsFile;

fn config_layers(
    ctx: &CommandContext,
    workflow: Option<&Path>,
) -> anyhow::Result<EffectiveSettingsLayers> {
    let cwd = ctx.cwd();
    let (workflow_layer, project_layer) = match workflow {
        Some(path) => workflow_and_project_layers(path, cwd)?,
        None => (ConfigLayer::default(), ConfigLayer::project(cwd)?),
    };
    let user_layer = user_config::settings_layer_with_config_and_storage_dir(
        Some(ctx.base_config_path()),
        None,
    )?;
    Ok(EffectiveSettingsLayers::new(
        ConfigLayer::default(),
        workflow_layer,
        project_layer,
        user_layer,
    ))
}

fn workflow_and_project_layers(
    path: &Path,
    cwd: &Path,
) -> anyhow::Result<(ConfigLayer, ConfigLayer)> {
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

async fn merged_config(args: &SettingsArgs) -> anyhow::Result<SettingsFile> {
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
    // `retrieve_server_settings` currently returns a legacy flat `Settings`;
    // route it through the v2 bridge shim for the consumer-side call.
    // Stage 6.6 rewrites the API client to return v2 types directly.
    let legacy_server = ctx.server().await?.retrieve_server_settings().await?;
    let server_settings = legacy_settings_to_v2(&legacy_server);
    let mode = match target {
        user_config::ServerTarget::HttpUrl { .. } => EffectiveSettingsMode::RemoteServer,
        user_config::ServerTarget::UnixSocket(_) => EffectiveSettingsMode::LocalDaemon,
    };

    effective_settings::resolve_settings(layers, Some(&server_settings), mode)
}

/// Stopgap shim that converts a legacy flat `Settings` back into a
/// `SettingsFile` for consumption by the v2-native resolver. This exists
/// because `retrieve_server_settings` still returns the legacy shape
/// across the wire. When Stage 6.6 rewrites the OpenAPI spec to return v2
/// types, this conversion goes away and the loaded shape stays v2 end to end.
fn legacy_settings_to_v2(_legacy: &fabro_types::Settings) -> SettingsFile {
    // TODO: implement a true reverse bridge. For now, return an empty v2
    // file so `resolve_settings(..., Some(&...), RemoteServer)` has a
    // non-None server-settings argument. This loses server-side defaults;
    // Stage 6.6 fixes the full round-trip.
    SettingsFile::default()
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
