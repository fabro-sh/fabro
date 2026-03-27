use std::path::Path;

use anyhow::bail;
use fabro_util::terminal::Styles;

use crate::args::PreflightArgs;
use crate::cli_config;

use super::run::execute::{
    apply_execution_overrides, load_workflow_source_input, print_workflow_report,
    resolve_sandbox_provider, run_preflight, ExecutionOverrides,
};

pub async fn execute(mut args: PreflightArgs) -> anyhow::Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let cli_config = cli_config::load_cli_config(None)?;
    args.verbose = args.verbose || cli_config.verbose_enabled();

    let github_app = crate::shared::github::build_github_app_credentials(cli_config.app_id());

    let run_defaults = cli_config;

    let source_input = load_workflow_source_input(
        &args.workflow,
        args.goal.as_deref(),
        args.goal_file.as_deref(),
        run_defaults,
        true,
    )?;

    let original_cwd = std::env::current_dir()?;
    let (origin_url, detected_base_branch) =
        fabro_sandbox::daytona::detect_repo_info(&original_cwd)
            .map(|(url, branch)| (Some(url), branch))
            .unwrap_or((None, None));
    let git_status =
        fabro_workflows::git::sync_status(&original_cwd, "origin", detected_base_branch.as_deref());

    let sandbox_provider = resolve_sandbox_provider(
        args.sandbox.map(Into::into),
        Some(&source_input.config),
        &source_input.run_defaults,
    )?;

    let mut config = source_input.config.clone();
    apply_execution_overrides(
        &mut config,
        &ExecutionOverrides {
            dry_run: false,
            auto_approve: false,
            no_retro: false,
            verbose: args.verbose,
            preserve_sandbox: false,
            model: args.model.as_deref(),
            provider: args.provider.as_deref(),
            sandbox_provider,
        },
    );

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
            config: Some(config.clone()),
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
        &Some(config),
        args.model.as_deref(),
        args.provider.as_deref(),
        &source_input.run_defaults,
        git_status,
        sandbox_provider,
        styles,
        github_app,
        origin_url.as_deref(),
    )
    .await
}
