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
use fabro_graphviz::render;
use fabro_manifest::{ManifestBuildInput, build_run_manifest};
use fabro_util::terminal::Styles;
use tracing::debug;

use crate::args::{GraphArgs, GraphDirection, GraphOutputFormat};
use crate::command_context::CommandContext;
use crate::commands::run::output::api_diagnostics_to_local;
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
        workflow: args.workflow.clone(),
        cwd: ctx.cwd().to_path_buf(),
        environment_defaults: fabro_environment::seeded_catalog_layer(),
        user_settings_path: Some(active_settings_path(None)),
        ..Default::default()
    })?;
    let client = ctx.server().await?;
    let preflight = client.run_preflight(built.manifest.clone()).await?;
    let diagnostics = api_diagnostics_to_local(&preflight.workflow.diagnostics);

    print_diagnostics(&diagnostics, styles, printer);
    let has_errors = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error);
    if has_errors && !args.allow_invalid {
        bail!("Validation failed");
    }

    let rendered = if has_errors && args.allow_invalid {
        render_manifest_graph_locally(&built.manifest, args.direction)?
    } else {
        client
            .render_workflow_graph(types::RenderWorkflowGraphRequest {
                manifest:  built.manifest,
                format:    Some(types::RenderWorkflowGraphFormat::Svg),
                direction: args.direction.map(|direction| match direction {
                    GraphDirection::Lr => types::RenderWorkflowGraphDirection::Lr,
                    GraphDirection::Tb => types::RenderWorkflowGraphDirection::Tb,
                }),
            })
            .await?
    };

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

fn render_manifest_graph_locally(
    manifest: &types::RunManifest,
    direction: Option<GraphDirection>,
) -> anyhow::Result<Vec<u8>> {
    let source = manifest_root_source(manifest)?;
    let source = match direction {
        Some(direction) => render::apply_direction(source, &direction.to_string()).into_owned(),
        None => source.to_string(),
    };

    render::render_dot(&source).context("rendering workflow graph")
}

fn manifest_root_source(manifest: &types::RunManifest) -> anyhow::Result<&str> {
    manifest
        .workflows
        .get(&manifest.target.path)
        .map(|workflow| workflow.source.as_str())
        .with_context(|| {
            format!(
                "manifest target path is missing from workflows map: {}",
                manifest.target.path
            )
        })
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
