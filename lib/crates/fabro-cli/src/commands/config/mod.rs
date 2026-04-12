use std::io::Write;
use std::path::Path;

use fabro_config::effective_settings::{EffectiveSettingsLayers, EffectiveSettingsMode};
use fabro_config::{load_and_resolve, load_settings_project, project};
use fabro_types::settings::SettingsLayer;
use fabro_util::printer::Printer;

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

fn strip_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                strip_nulls(child);
            }
            map.retain(|_, child| !child.is_null());
        }
        serde_json::Value::Array(values) => {
            for child in values {
                strip_nulls(child);
            }
        }
        _ => {}
    }
}

fn local_settings_value(
    args: &SettingsArgs,
    printer: Printer,
) -> anyhow::Result<serde_json::Value> {
    let base_ctx = CommandContext::base(printer)?;
    let layers = config_layers(&base_ctx, args.workflow.as_deref())?;
    let mut value = serde_json::to_value(load_and_resolve(
        layers,
        None,
        EffectiveSettingsMode::LocalOnly,
    )?)?;
    strip_nulls(&mut value);
    Ok(value)
}

async fn rendered_config(
    args: &SettingsArgs,
    printer: Printer,
) -> anyhow::Result<serde_json::Value> {
    if args.local {
        return local_settings_value(args, printer);
    }
    if args.workflow.is_some() {
        anyhow::bail!("WORKFLOW requires --local; use `fabro settings --local WORKFLOW`");
    }
    let ctx = CommandContext::for_target(&args.target, printer)?;
    ctx.server()
        .await?
        .retrieve_resolved_server_settings()
        .await
}

pub(crate) async fn execute(
    args: &SettingsArgs,
    globals: &GlobalArgs,
    printer: Printer,
) -> anyhow::Result<()> {
    let config = Box::pin(rendered_config(args, printer)).await?;
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
