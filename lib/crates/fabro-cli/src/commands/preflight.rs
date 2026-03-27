use std::path::Path;

use anyhow::bail;
use fabro_config::{FabroConfig, FabroSettings};
use fabro_util::terminal::Styles;

use super::run::execute::{
    load_workflow_source_input, print_workflow_report, resolve_sandbox_provider, run_preflight,
};
use crate::args::PreflightArgs;

pub async fn execute(mut args: PreflightArgs) -> anyhow::Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let cli_defaults = fabro_config::cli::load_cli_config(None)?;
    let cli_config: FabroSettings = cli_defaults.clone().try_into()?;
    args.verbose = args.verbose || cli_config.verbose_enabled();

    let github_app = crate::shared::github::build_github_app_credentials(cli_config.app_id());
    let cli_args_config = FabroConfig::try_from(&args)?;

    let source_input =
        load_workflow_source_input(&args.workflow, cli_args_config, cli_defaults, true)?;

    let original_cwd = std::env::current_dir()?;
    let (origin_url, detected_base_branch) =
        fabro_sandbox::daytona::detect_repo_info(&original_cwd)
            .map(|(url, branch)| (Some(url), branch))
            .unwrap_or((None, None));
    let git_status =
        fabro_workflows::git::sync_status(&original_cwd, "origin", detected_base_branch.as_deref());

    let sandbox_provider =
        resolve_sandbox_provider(args.sandbox.map(Into::into), &source_input.settings)?;

    let validated = fabro_workflows::operations::validate(
        &source_input.raw_source,
        fabro_workflows::operations::ValidateOptions {
            base_dir: Some(
                source_input
                    .dot_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_path_buf(),
            ),
            settings: Some(source_input.settings.clone()),
            goal_override: source_input.goal_override.clone(),
            ..Default::default()
        },
    )?;
    print_workflow_report(&validated, &source_input.dot_path, styles);
    if validated.has_errors() {
        bail!("Validation failed");
    }

    run_preflight(
        validated.graph(),
        &source_input.settings,
        args.model.as_deref(),
        args.provider.as_deref(),
        git_status,
        sandbox_provider,
        styles,
        github_app,
        origin_url.as_deref(),
    )
    .await
}
