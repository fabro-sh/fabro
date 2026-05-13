use anyhow::bail;
use fabro_config::RunLayer;
use fabro_config::user::active_settings_path;
use fabro_manifest::{ManifestBuildInput, build_run_manifest};
use fabro_server::manifest_validation;
use fabro_util::terminal::Styles;
use fabro_workflow::operations::RenderMode;

use crate::args::ValidateArgs;
use crate::command_context::CommandContext;
use crate::commands::run::output::api_diagnostics_to_local;
use crate::shared::{print_diagnostics, print_json_pretty, relative_path};

pub(crate) fn run(
    args: &ValidateArgs,
    styles: &Styles,
    base_ctx: &CommandContext,
) -> anyhow::Result<()> {
    let printer = base_ctx.printer();
    let built = build_run_manifest(ManifestBuildInput {
        workflow: args.workflow.clone(),
        cwd: base_ctx.cwd().to_path_buf(),
        user_settings_path: Some(active_settings_path(None)),
        ..Default::default()
    })?;
    let response = manifest_validation::validate_manifest(
        &RunLayer::default(),
        &built.manifest,
        RenderMode::Structural,
        base_ctx.catalog()?,
    )?;
    let diagnostics = api_diagnostics_to_local(&response.workflow.diagnostics);

    if base_ctx.json_output() {
        print_json_pretty(&serde_json::json!({
            "workflow_name": response.workflow.name,
            "nodes": response.workflow.nodes,
            "edges": response.workflow.edges,
            "valid": !diagnostics.iter().any(|d| d.severity == fabro_validate::Severity::Error),
            "diagnostics": diagnostics,
        }))?;

        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
        {
            bail!("Validation failed");
        }
        return Ok(());
    }

    fabro_util::printerr!(
        printer,
        "{} ({} nodes, {} edges)",
        styles
            .bold
            .apply_to(format!("Workflow: {}", response.workflow.name)),
        response.workflow.nodes,
        response.workflow.edges,
    );
    fabro_util::printerr!(
        printer,
        "{} {}",
        styles.dim.apply_to("Graph:"),
        styles.dim.apply_to(relative_path(&built.target_path)),
    );

    print_diagnostics(&diagnostics, styles, printer);

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
    {
        bail!("Validation failed");
    }

    fabro_util::printerr!(printer, "Validation: {}", styles.green.apply_to("OK"));
    Ok(())
}
