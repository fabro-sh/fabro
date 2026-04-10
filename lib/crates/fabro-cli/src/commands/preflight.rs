use anyhow::bail;
use fabro_types::settings::cli::OutputVerbosity;
use fabro_util::terminal::Styles;

use crate::args::{GlobalArgs, PreflightArgs};
use crate::command_context::CommandContext;
use crate::commands::run::output::{
    api_check_report_to_local, api_diagnostics_to_local, print_preflight_workflow_summary,
};
use crate::commands::run::overrides::preflight_args_layer;
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest, preflight_manifest_args};
use crate::shared::print_json_pretty;

pub(crate) async fn execute(mut args: PreflightArgs, globals: &GlobalArgs) -> anyhow::Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let ctx = CommandContext::for_target(&args.target)?;
    args.verbose = args.verbose || ctx.cli_settings().output.verbosity == OutputVerbosity::Verbose;

    let manifest = build_run_manifest(ManifestBuildInput {
        workflow: args.workflow.clone(),
        cwd: ctx.cwd().to_path_buf(),
        args_layer: preflight_args_layer(&args)?,
        args: preflight_manifest_args(&args),
        run_id: None,
    })?;
    let client = ctx.server().await?;
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
