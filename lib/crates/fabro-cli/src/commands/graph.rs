use std::io::Write;

use anyhow::bail;
use fabro_api::types;
use fabro_config::load::load_settings_user;
use fabro_config::user::active_settings_path;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_types::settings::{CliSettings, SettingsLayer};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use tracing::debug;

use crate::args::{GraphArgs, GraphDirection, require_no_json_override};
use crate::command_context::CommandContext;
use crate::commands::run::output::api_diagnostics_to_local;
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest};
use crate::shared::{absolute_or_current, print_diagnostics, print_json_pretty, relative_path};

pub(crate) async fn run(
    args: &GraphArgs,
    styles: &Styles,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    process_local_json: bool,
    printer: Printer,
) -> anyhow::Result<()> {
    if process_local_json && args.output.is_none() {
        require_no_json_override(process_local_json)?;
    }

    let ctx = CommandContext::for_target(&args.target, printer, cli.clone(), cli_layer)?;
    let built = build_run_manifest(ManifestBuildInput {
        workflow: args.workflow.clone(),
        cwd: ctx.cwd().to_path_buf(),
        args_layer: SettingsLayer::default(),
        args: None,
        run_id: None,
        user_layer: load_settings_user()?,
        user_settings_path: Some(active_settings_path(None)),
    })?;
    let client = ctx.server().await?;
    let preflight = client.run_preflight(built.manifest.clone()).await?;
    let diagnostics = api_diagnostics_to_local(&preflight.workflow.diagnostics);

    print_diagnostics(&diagnostics, styles, printer);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
    {
        bail!("Validation failed");
    }

    let rendered = client
        .render_workflow_graph(types::RenderWorkflowGraphRequest {
            manifest: built.manifest,
            format: Some(types::RenderWorkflowGraphFormat::Svg),
            direction: args.direction.map(|direction| match direction {
                GraphDirection::Lr => types::RenderWorkflowGraphDirection::Lr,
                GraphDirection::Tb => types::RenderWorkflowGraphDirection::Tb,
            }),
        })
        .await?;

    if let Some(ref output_path) = args.output {
        std::fs::write(output_path, &rendered)?;
        if cli.output.format == OutputFormat::Json {
            print_json_pretty(&serde_json::json!({
                "path": absolute_or_current(output_path),
                "format": args.format.to_string(),
            }))?;
        }
    } else {
        std::io::stdout().write_all(&rendered)?;
    }

    debug!(
        path = %relative_path(&built.target_path),
        format = %args.format,
        "Rendered workflow graph"
    );

    Ok(())
}
