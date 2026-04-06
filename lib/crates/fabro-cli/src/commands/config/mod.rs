use std::io::Write;
use std::path::Path;

use crate::args::{GlobalArgs, SettingsArgs};
use crate::server_client;
use crate::shared::print_json_pretty;
use crate::user_config;
use fabro_config::ConfigLayer;
use fabro_config::effective_settings;
use fabro_config::effective_settings::{EffectiveSettingsLayers, EffectiveSettingsMode};
use fabro_config::project;
use fabro_types::Settings;

fn config_layers(workflow: Option<&Path>) -> anyhow::Result<EffectiveSettingsLayers> {
    let cwd = std::env::current_dir()?;
    let (workflow_layer, project_layer) = match workflow {
        Some(path) => workflow_and_project_layers(path, &cwd)?,
        None => (ConfigLayer::default(), ConfigLayer::project(&cwd)?),
    };
    let user_layer = user_config::settings_layer_with_storage_dir(None)?;
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

async fn merged_config(args: &SettingsArgs) -> anyhow::Result<Settings> {
    let layers = config_layers(args.workflow.as_deref())?;
    if args.local {
        return effective_settings::resolve_settings(
            layers,
            None,
            EffectiveSettingsMode::LocalOnly,
        );
    }

    let machine_settings = user_config::load_settings()?;
    let target = user_config::resolve_server_target(&args.target, &machine_settings)?;
    let client = server_client::connect_server_only(&args.target).await?;
    let server_settings = client.retrieve_server_settings().await?;
    let mode = match target {
        user_config::ServerTarget::HttpUrl { .. } => EffectiveSettingsMode::RemoteServer,
        user_config::ServerTarget::UnixSocket(_) => EffectiveSettingsMode::LocalDaemon,
    };

    effective_settings::resolve_settings(layers, Some(&server_settings), mode)
}

pub(crate) async fn execute(args: &SettingsArgs, globals: &GlobalArgs) -> anyhow::Result<()> {
    let config = merged_config(args).await?;
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
