use std::path::PathBuf;

use fabro_config::config::FabroConfig;
use fabro_sandbox::SandboxProvider;

use crate::args::RunArgs;

use super::execute::{
    apply_execution_overrides, cached_graph_path, default_run_dir, load_workflow_source_input,
    parse_labels, print_diagnostics_from_error, print_workflow_report_from_persisted,
    resolve_sandbox_provider, write_run_config_snapshot, ExecutionOverrides,
};
use fabro_util::terminal::Styles;

/// Create a workflow run: allocate run directory, persist RunRecord, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub async fn create_run(
    args: &RunArgs,
    run_defaults: FabroConfig,
    styles: &Styles,
    quiet: bool,
) -> anyhow::Result<(String, PathBuf)> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;
    let source_input = load_workflow_source_input(
        workflow_path,
        args.goal.as_deref(),
        args.goal_file.as_deref(),
        run_defaults,
        true,
    )?;
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    let run_dir = args
        .run_dir
        .clone()
        .unwrap_or_else(|| default_run_dir(&run_id, args.dry_run));
    let working_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let base_branch = fabro_sandbox::daytona::detect_repo_info(&working_directory)
        .ok()
        .and_then(|(_, branch)| branch);
    let sandbox_provider = if args.dry_run {
        SandboxProvider::Local
    } else {
        resolve_sandbox_provider(
            args.sandbox.map(Into::into),
            Some(&source_input.config),
            &source_input.run_defaults,
        )?
    };

    let mut config = source_input.config.clone();
    apply_execution_overrides(
        &mut config,
        &ExecutionOverrides {
            dry_run: args.dry_run,
            auto_approve: args.auto_approve,
            no_retro: args.no_retro,
            verbose: args.verbose,
            preserve_sandbox: args.preserve_sandbox,
            model: args.model.as_deref(),
            provider: args.provider.as_deref(),
            sandbox_provider,
        },
    );

    let persisted = match fabro_workflows::operations::create(
        &source_input.raw_source,
        fabro_workflows::operations::RunCreateOptions {
            config,
            run_dir: Some(run_dir.clone()),
            run_id: Some(run_id.clone()),
            workflow_slug: source_input.workflow_slug.clone(),
            labels: parse_labels(&args.label),
            base_branch,
            working_directory: Some(working_directory.clone()),
            host_repo_path: Some(working_directory.to_string_lossy().to_string()),
            goal_override: source_input.goal_override.clone(),
            base_dir: Some(
                source_input
                    .dot_path
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .to_path_buf(),
            ),
        },
    ) {
        Ok(persisted) => persisted,
        Err(fabro_workflows::error::FabroError::ValidationFailed { diagnostics }) => {
            if !quiet {
                print_diagnostics_from_error(&diagnostics, styles);
            }
            anyhow::bail!("Validation failed");
        }
        Err(err) => return Err(err.into()),
    };

    if !quiet {
        print_workflow_report_from_persisted(&persisted, &source_input.dot_path, styles);
    }

    // Write CLI-owned debug and status artifacts after the run has been persisted.
    tokio::fs::write(cached_graph_path(&run_dir), &source_input.raw_source).await?;
    tokio::fs::write(run_dir.join("id.txt"), &run_id).await?;
    std::fs::File::create(run_dir.join("progress.jsonl"))?;
    fabro_workflows::run_status::write_run_status(
        &run_dir,
        fabro_workflows::run_status::RunStatus::Submitted,
        None,
    );
    write_run_config_snapshot(&run_dir, source_input.workflow_toml_path.as_deref()).await?;

    Ok((run_id, run_dir))
}
