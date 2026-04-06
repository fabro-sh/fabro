use anyhow::bail;
use fabro_config::ConfigLayer;
use fabro_util::terminal::Styles;

use crate::args::{GlobalArgs, PreflightArgs};
use crate::commands::run::output::{
    api_check_report_to_local, api_diagnostics_to_local, print_preflight_workflow_summary,
};
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest, preflight_manifest_args};
use crate::server_client;
use crate::shared::print_json_pretty;
use crate::user_config::{self, load_settings_with_storage_dir};

pub(crate) async fn execute(mut args: PreflightArgs, globals: &GlobalArgs) -> anyhow::Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let cli_settings = load_settings_with_storage_dir(args.target.storage_dir())?;
    args.verbose = args.verbose || cli_settings.verbose_enabled();
    let connection = user_config::server_backed_command_connection(&args.target, &cli_settings)?;

    let cwd = std::env::current_dir()?;
    let manifest = build_run_manifest(ManifestBuildInput {
        workflow: args.workflow.clone(),
        cwd,
        args_layer: ConfigLayer::try_from(&args)?,
        args: preflight_manifest_args(&args),
        run_id: None,
    })?;
    let client = server_client::connect_server_connection(&connection).await?;
    let response = client.run_preflight(manifest.manifest).await?;
    let diagnostics = api_diagnostics_to_local(&response.workflow.diagnostics);

    if globals.json {
        print_json_pretty(&response)?;
    } else {
        print_preflight_workflow_summary(&response.workflow, Some(&manifest.target_path), styles);
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
        {
            bail!("Validation failed");
        }
        let report = api_check_report_to_local(&response.checks);
        let term_width = console::Term::stderr().size().1;
        print!("{}", report.render(styles, true, None, Some(term_width)));
    }

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
    {
        bail!("Validation failed");
    }
    if !response.ok {
        std::process::exit(1);
    }

    Ok(())
}
