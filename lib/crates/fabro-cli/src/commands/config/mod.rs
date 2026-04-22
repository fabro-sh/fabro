#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `config` command: blocking std::io::Write is the intended output mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `config` command: blocking std::io::stdout is the intended output mechanism"
)]

use std::io::Write;
use std::path::Path;

use fabro_config::effective_settings::{EffectiveSettingsLayers, EffectiveSettingsMode};
use fabro_config::{load_settings_project, project};
use fabro_types::settings::Settings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_types::settings::{CliSettings, SettingsLayer};
use fabro_util::printer::Printer;

use crate::args::SettingsArgs;
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
    let user_layer =
        user_config::load_settings_with_config_and_storage_dir(Some(ctx.base_config_path()), None)?;
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
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> anyhow::Result<serde_json::Value> {
    let base_ctx = CommandContext::base(printer, cli.clone(), cli_layer)?;
    let layers = config_layers(&base_ctx, args.workflow.as_deref())?;
    let local_settings = fabro_config::effective_settings::materialize_settings_layer(
        layers,
        None,
        EffectiveSettingsMode::LocalOnly,
    )?;
    let mut value = serde_json::to_value(resolve_local_settings(&local_settings, printer)?)?;
    strip_nulls(&mut value);
    Ok(value)
}

fn render_resolve_errors(errors: Vec<fabro_config::ResolveError>) -> anyhow::Error {
    anyhow::anyhow!(
        "failed to resolve local settings:\n{}",
        errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn resolve_local_settings(file: &SettingsLayer, printer: Printer) -> anyhow::Result<Settings> {
    let file = fabro_config::apply_builtin_defaults(file.clone());

    let project = fabro_config::resolve_project_from_file(&file).map_err(render_resolve_errors)?;
    let workflow = fabro_config::resolve_workflow_from_file(&file).map_err(render_resolve_errors)?;
    let run = fabro_config::resolve_run_from_file(&file).map_err(render_resolve_errors)?;
    let cli = fabro_config::resolve_cli_from_file(&file).map_err(render_resolve_errors)?;
    let features = fabro_config::resolve_features_from_file(&file).map_err(render_resolve_errors)?;

    let server = match fabro_config::resolve_server_from_file(&file) {
        Ok(server) => server,
        Err(errors) => {
            for error in &errors {
                fabro_util::printerr!(printer, "warning: server config: {error}");
            }

            let server_layer = file.server.clone().unwrap_or_default();
            let mut ignored = Vec::new();
            fabro_config::resolve_server(&server_layer, &mut ignored)
        }
    };

    Ok(Settings {
        project,
        workflow,
        run,
        cli,
        server,
        features,
    })
}

async fn rendered_config(
    args: &SettingsArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> anyhow::Result<serde_json::Value> {
    if args.local {
        return local_settings_value(args, cli, cli_layer, printer);
    }
    if args.workflow.is_some() {
        anyhow::bail!("WORKFLOW requires --local; use `fabro settings --local WORKFLOW`");
    }
    let ctx = CommandContext::for_target(&args.target, printer, cli.clone(), cli_layer)?;
    ctx.server()
        .await?
        .retrieve_resolved_server_settings()
        .await
}

pub(crate) async fn execute(
    args: &SettingsArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> anyhow::Result<()> {
    let config = Box::pin(rendered_config(args, cli, cli_layer, printer)).await?;
    if cli.output.format == OutputFormat::Json {
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
