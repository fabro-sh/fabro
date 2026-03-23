use std::path::PathBuf;

use chrono::Utc;
use fabro_config::run::RunDefaults;
use fabro_workflows::manifest::Manifest;
use fabro_workflows::run_spec::RunSpec;

use super::run::{
    cached_graph_path, default_run_dir, prepare_workflow, write_run_config_snapshot, RunArgs,
};
use fabro_util::terminal::Styles;

/// Create a workflow run: allocate run directory, persist spec, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub async fn create_run(
    args: &RunArgs,
    run_defaults: RunDefaults,
    styles: &Styles,
    quiet: bool,
) -> anyhow::Result<(String, PathBuf)> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;

    let mut prep = prepare_workflow(args, run_defaults, styles, quiet)?;

    let goal = prep.graph.goal();

    // Create run directory
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    let run_dir = args
        .run_dir
        .clone()
        .unwrap_or_else(|| default_run_dir(&run_id, args.dry_run));
    tokio::fs::create_dir_all(&run_dir).await?;

    // Write essential files
    tokio::fs::write(cached_graph_path(&run_dir), &prep.source).await?;
    tokio::fs::write(run_dir.join("id.txt"), &run_id).await?;
    std::fs::File::create(run_dir.join("progress.jsonl"))?;
    fabro_workflows::run_status::write_run_status(
        &run_dir,
        fabro_workflows::run_status::RunStatus::Submitted,
        None,
    );

    // Serialize the merged run config so the run dir is self-contained.
    write_run_config_snapshot(&run_dir, prep.run_cfg.as_mut()).await?;

    // Build and save RunSpec
    let working_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let labels: std::collections::HashMap<String, String> = args
        .label
        .iter()
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let spec = RunSpec {
        run_id: run_id.clone(),
        workflow_path: std::fs::canonicalize(workflow_path).unwrap_or(workflow_path.clone()),
        dot_source: prep.source,
        working_directory: working_directory.clone(),
        goal: if goal.is_empty() {
            None
        } else {
            Some(goal.to_string())
        },
        model: prep.model,
        provider: prep.provider,
        sandbox_provider: prep.sandbox_provider.to_string(),
        labels: labels.clone(),
        verbose: args.verbose,
        no_retro: args.no_retro,
        preserve_sandbox: args.preserve_sandbox,
        dry_run: args.dry_run,
        auto_approve: args.auto_approve,
    };
    spec.save(&run_dir)?;

    let workflow_name = if prep.graph.name.is_empty() {
        "unnamed".to_string()
    } else {
        prep.graph.name.clone()
    };
    let base_branch = fabro_sandbox::daytona::detect_repo_info(&working_directory)
        .ok()
        .and_then(|(_, branch)| branch);
    let manifest = Manifest {
        run_id: run_id.clone(),
        workflow_name,
        goal: goal.to_string(),
        start_time: Utc::now(),
        node_count: prep.graph.nodes.len(),
        edge_count: prep.graph.edges.len(),
        run_branch: None,
        base_sha: None,
        labels,
        base_branch,
        workflow_slug: prep.workflow_slug.clone(),
        host_repo_path: Some(working_directory.to_string_lossy().to_string()),
    };
    manifest.save(&run_dir.join("manifest.json"))?;

    Ok((run_id, run_dir))
}
