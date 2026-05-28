use anyhow::bail;
use fabro_config::user::active_settings_path;
use fabro_manifest::{ManifestBuildInput, build_run_manifest};
use fabro_util::terminal::Styles;

use crate::args::PreflightArgs;
use crate::command_context::CommandContext;
use crate::commands::run::output::{
    api_check_report_to_local, api_diagnostics_to_local, print_workflow_summary,
};
use crate::commands::run::overrides::preflight_args_overrides;
use crate::manifest_args::preflight_manifest_args;
use crate::shared::{cyan_spinner, print_json_pretty};

pub(crate) async fn execute(
    mut args: PreflightArgs,
    base_ctx: &CommandContext,
) -> anyhow::Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.target)?;
    args.verbose = args.verbose || ctx.verbose();
    let cli_args_config = preflight_args_overrides(&args)?;

    let manifest = build_run_manifest(ManifestBuildInput {
        workflow: args.workflow.clone(),
        cwd: ctx.cwd().to_path_buf(),
        run_overrides: cli_args_config.run,
        cli_overrides: cli_args_config.cli,
        input_overrides: cli_args_config.input_overrides,
        args: preflight_manifest_args(&args),
        environment_defaults: fabro_environment::seeded_catalog_layer(),
        user_settings_path: Some(active_settings_path(None)),
        ..Default::default()
    })?;

    let spinner = (!ctx.json_output()).then(|| cyan_spinner("Running checks..."));

    let result = async {
        let client = ctx.server().await?;
        client.run_preflight(manifest.manifest).await
    }
    .await;
    if let Some(spinner) = spinner.as_ref() {
        spinner.finish_and_clear();
    }
    let response = result?;
    let diagnostics = api_diagnostics_to_local(&response.workflow.diagnostics);

    if ctx.json_output() {
        print_json_pretty(&response)?;
    } else {
        print_workflow_summary(
            &response.workflow,
            Some(&manifest.target_path),
            styles,
            printer,
        );
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == fabro_validate::Severity::Error)
        {
            bail!("Validation failed");
        }
        let report = api_check_report_to_local(&response.checks);
        let term_width = console::Term::stderr().size().1;
        {
            use std::fmt::Write as _;
            let _ = write!(
                printer.stdout(),
                "{}",
                report.render(styles, true, None, Some(term_width))
            );
        }
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
