use std::io::Write;

use anyhow::bail;
use fabro_api::types;
use fabro_types::settings::SettingsFile;
use fabro_util::terminal::Styles;
use tracing::debug;

use crate::args::{GlobalArgs, GraphArgs, GraphDirection, GraphOutputFormat};
use crate::command_context::CommandContext;
use crate::commands::run::output::api_diagnostics_to_local;
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest};
use crate::shared::{absolute_or_current, print_diagnostics, print_json_pretty, relative_path};

pub(crate) async fn run(
    args: &GraphArgs,
    styles: &Styles,
    globals: &GlobalArgs,
) -> anyhow::Result<()> {
    if globals.json && args.output.is_none() {
        globals.require_no_json()?;
    }

    let ctx = CommandContext::for_target(&args.target)?;
    let built = build_run_manifest(ManifestBuildInput {
        workflow: args.workflow.clone(),
        cwd: ctx.cwd().to_path_buf(),
        args_layer: SettingsFile::default(),
        args: None,
        run_id: None,
    })?;
    let client = ctx.server().await?;
    let preflight = client.run_preflight(built.manifest.clone()).await?;
    let diagnostics = api_diagnostics_to_local(&preflight.workflow.diagnostics);

    print_diagnostics(&diagnostics, styles);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
    {
        bail!("Validation failed");
    }

    let rendered = client
        .render_workflow_graph(types::RenderWorkflowGraphRequest {
            manifest: built.manifest,
            format: Some(match args.format {
                GraphOutputFormat::Svg => types::RenderWorkflowGraphFormat::Svg,
                GraphOutputFormat::Png => types::RenderWorkflowGraphFormat::Png,
            }),
            direction: args.direction.map(|direction| match direction {
                GraphDirection::Lr => types::RenderWorkflowGraphDirection::Lr,
                GraphDirection::Tb => types::RenderWorkflowGraphDirection::Tb,
            }),
        })
        .await?;

    if let Some(ref output_path) = args.output {
        std::fs::write(output_path, &rendered)?;
        if globals.json {
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
