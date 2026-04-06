use anyhow::bail;
use fabro_config::ConfigLayer;
use fabro_util::terminal::Styles;

use crate::args::{GlobalArgs, ValidateArgs};
use crate::commands::run::output::api_diagnostics_to_local;
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest};
use crate::server_client;
use crate::shared::{print_diagnostics, print_json_pretty, relative_path};

pub(crate) async fn run(
    args: &ValidateArgs,
    styles: &Styles,
    globals: &GlobalArgs,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let built = build_run_manifest(ManifestBuildInput {
        workflow: args.workflow.clone(),
        cwd,
        args_layer: ConfigLayer::default(),
        args: None,
        run_id: None,
    })?;
    let client = server_client::connect_server_only(&args.target).await?;
    let response = client.run_preflight(built.manifest).await?;
    let diagnostics = api_diagnostics_to_local(&response.workflow.diagnostics);

    if globals.json {
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

    eprintln!(
        "{} ({} nodes, {} edges)",
        styles
            .bold
            .apply_to(format!("Workflow: {}", response.workflow.name)),
        response.workflow.nodes,
        response.workflow.edges,
    );
    eprintln!(
        "{} {}",
        styles.dim.apply_to("Graph:"),
        styles.dim.apply_to(relative_path(&built.target_path)),
    );

    print_diagnostics(&diagnostics, styles);

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
    {
        bail!("Validation failed");
    }

    eprintln!("Validation: {}", styles.green.apply_to("OK"));
    Ok(())
}
