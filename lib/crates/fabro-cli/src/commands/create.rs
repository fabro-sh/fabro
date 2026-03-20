use std::path::PathBuf;

use chrono::Local;
use fabro_config::run::RunDefaults;
use fabro_workflows::run_spec::RunSpec;

use super::run::{prepare_workflow, RunArgs};
use fabro_util::terminal::Styles;

/// Create a workflow run: allocate run directory, persist spec, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub async fn create_run(
    args: &RunArgs,
    run_defaults: RunDefaults,
    styles: &Styles,
) -> anyhow::Result<(String, PathBuf)> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;

    let prep = prepare_workflow(args, run_defaults, styles)?;

    let goal = prep.graph.goal();

    // Create run directory
    let run_id = ulid::Ulid::new().to_string();
    let run_dir = args.run_dir.clone().unwrap_or_else(|| {
        if args.dry_run {
            std::env::temp_dir().join("fabro-dry-run").join(&run_id)
        } else {
            let base = dirs::home_dir()
                .expect("could not determine home directory")
                .join(".fabro")
                .join("runs");
            base.join(format!("{}-{}", Local::now().format("%Y%m%d"), run_id))
        }
    });
    tokio::fs::create_dir_all(&run_dir).await?;

    // Write essential files
    tokio::fs::write(run_dir.join("graph.fabro"), &prep.source).await?;
    tokio::fs::write(run_dir.join("id.txt"), &run_id).await?;
    std::fs::File::create(run_dir.join("progress.jsonl"))?;
    fabro_workflows::run_status::write_run_status(
        &run_dir,
        fabro_workflows::run_status::RunStatus::Submitted,
        None,
    );

    // Save TOML config alongside the run if present
    if workflow_path.extension().is_some_and(|ext| ext == "toml") {
        if let Ok(toml_contents) = tokio::fs::read(workflow_path).await {
            tokio::fs::write(run_dir.join("run.toml"), toml_contents).await?;
        }
    }

    // Build and save RunSpec
    let working_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let spec = RunSpec {
        run_id: run_id.clone(),
        workflow_path: std::fs::canonicalize(workflow_path).unwrap_or(workflow_path.clone()),
        dot_source: prep.source,
        working_directory,
        goal: if goal.is_empty() {
            None
        } else {
            Some(goal.to_string())
        },
        model: prep.model,
        provider: prep.provider,
        sandbox_provider: prep.sandbox_provider.to_string(),
        labels: args
            .label
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        verbose: args.verbose,
        no_retro: args.no_retro,
        ssh: args.ssh,
        preserve_sandbox: args.preserve_sandbox,
        dry_run: args.dry_run,
        auto_approve: args.auto_approve,
        resume: args.resume.clone(),
        run_branch: args.run_branch.clone(),
    };
    spec.save(&run_dir)?;

    Ok((run_id, run_dir))
}
