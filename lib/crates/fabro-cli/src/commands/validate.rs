use anyhow::bail;
use fabro_config::load::load_settings_user;
use fabro_config::user::active_settings_path;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_types::settings::{CliSettings, SettingsLayer};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::ValidateArgs;
use crate::command_context::CommandContext;
use crate::commands::run::output::api_diagnostics_to_local;
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest};
use crate::shared::{print_diagnostics, print_json_pretty, relative_path};

pub(crate) async fn run(
    args: &ValidateArgs,
    styles: &Styles,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> anyhow::Result<()> {
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
    let response = client.run_preflight(built.manifest).await?;
    let diagnostics = api_diagnostics_to_local(&response.workflow.diagnostics);

    if cli.output.format == OutputFormat::Json {
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
