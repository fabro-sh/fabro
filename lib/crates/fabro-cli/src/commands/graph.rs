#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `graph` command: blocking std::io::Write is the intended output mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `graph` command: blocking std::io::stdout is the intended output mechanism"
)]

use std::io::Write;

use anyhow::{Context, bail};
use fabro_api::types;
use fabro_config::user::active_settings_path;
use fabro_util::terminal::Styles;
use tracing::debug;

use crate::args::{GraphArgs, GraphDirection, GraphOutputFormat};
use crate::command_context::CommandContext;
use crate::commands::run::output::api_diagnostics_to_local;
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest};
use crate::shared::{absolute_or_current, print_diagnostics, print_json_pretty, relative_path};

pub(crate) async fn run(
    args: &GraphArgs,
    styles: &Styles,
    base_ctx: &CommandContext,
) -> anyhow::Result<()> {
    if args.output.is_none() {
        base_ctx.require_no_json_override()?;
    }

    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.target)?;
    let built = build_run_manifest(ManifestBuildInput {
        workflow:           args.workflow.clone(),
        cwd:                ctx.cwd().to_path_buf(),
        run_overrides:      None,
        cli_overrides:      None,
        args:               None,
        run_id:             None,
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
            manifest:  built.manifest,
            format:    Some(types::RenderWorkflowGraphFormat::Svg),
            direction: args.direction.map(|direction| match direction {
                GraphDirection::Lr => types::RenderWorkflowGraphDirection::Lr,
                GraphDirection::Tb => types::RenderWorkflowGraphDirection::Tb,
            }),
        })
        .await?;

    if let Some(ref output_path) = args.output {
        std::fs::write(output_path, &rendered)
            .with_context(|| format!("writing rendered graph to {}", output_path.display()))?;
        if ctx.json_output() {
            print_json_pretty(&output_file_json(output_path, args.format))?;
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

fn output_file_json(output_path: &std::path::Path, format: GraphOutputFormat) -> serde_json::Value {
    serde_json::json!({
        "path": absolute_or_current(output_path),
        "format": format.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_file_json_reports_absolute_path_and_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.svg");

        assert_eq!(
            output_file_json(&path, GraphOutputFormat::Svg),
            serde_json::json!({
                "path": path,
                "format": "svg",
            })
        );
    }
}
