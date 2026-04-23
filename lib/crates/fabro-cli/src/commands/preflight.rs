use anyhow::bail;
use fabro_config::load::load_settings_user;
use fabro_config::user::active_settings_path;
use fabro_util::terminal::Styles;

use crate::args::PreflightArgs;
use crate::command_context::CommandContext;
use crate::commands::run::output::{
    api_check_report_to_local, api_diagnostics_to_local, print_preflight_workflow_summary,
};
use crate::commands::run::overrides::preflight_args_layer;
use crate::manifest_builder::{ManifestBuildInput, build_run_manifest, preflight_manifest_args};
use crate::shared::print_json_pretty;

pub(crate) async fn execute(
    mut args: PreflightArgs,
    base_ctx: &CommandContext,
) -> anyhow::Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.target)?;
    args.verbose = args.verbose || ctx.verbose();

    let manifest = build_run_manifest(ManifestBuildInput {
        workflow:           args.workflow.clone(),
        cwd:                ctx.cwd().to_path_buf(),
        args_layer:         preflight_args_layer(&args)?,
        args:               preflight_manifest_args(&args),
        run_id:             None,
        user_layer:         load_settings_user()?,
        user_settings_path: Some(active_settings_path(None)),
    })?;
    let client = ctx.server().await?;
    let response = client.run_preflight(manifest.manifest).await?;
    let diagnostics = api_diagnostics_to_local(&response.workflow.diagnostics);

    if ctx.json_output() {
        print_json_pretty(&response)?;
    } else {
        print_preflight_workflow_summary(
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
