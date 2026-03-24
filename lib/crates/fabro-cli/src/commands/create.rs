use std::path::PathBuf;

use chrono::Utc;
use fabro_config::config::FabroConfig;
use fabro_workflows::run_record::RunRecord;
use fabro_workflows::sandbox_provider::SandboxProvider;

use super::run::{
    cached_graph_path, default_run_dir, prepare_workflow, write_run_config_snapshot, RunArgs,
};
use fabro_util::terminal::Styles;

/// CLI flag overrides for config normalization.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CliFlags {
    pub dry_run: bool,
    pub auto_approve: bool,
    pub no_retro: bool,
    pub verbose: bool,
    pub preserve_sandbox: bool,
}

impl From<&RunArgs> for CliFlags {
    fn from(args: &RunArgs) -> Self {
        Self {
            dry_run: args.dry_run,
            auto_approve: args.auto_approve,
            no_retro: args.no_retro,
            verbose: args.verbose,
            preserve_sandbox: args.preserve_sandbox,
        }
    }
}

/// Build a normalized FabroConfig that captures the full execution intent.
///
/// Folds resolved model/provider/sandbox/goal and CLI flag overrides back into
/// a single FabroConfig so the RunRecord is self-contained.
pub(crate) fn normalize_config(
    run_cfg: Option<&FabroConfig>,
    run_defaults: &FabroConfig,
    model: &str,
    provider: Option<&str>,
    sandbox_provider: SandboxProvider,
    graph: &fabro_graphviz::graph::Graph,
    flags: CliFlags,
) -> FabroConfig {
    let mut config = run_cfg.cloned().unwrap_or_else(|| run_defaults.clone());
    // Ensure resolved values are written back into config
    config.llm.get_or_insert_default().model = Some(model.to_string());
    config.llm.get_or_insert_default().provider = provider.map(String::from);
    config.sandbox.get_or_insert_default().provider = Some(sandbox_provider.to_string());
    let goal = graph.goal().to_string();
    config.goal = if goal.is_empty() { None } else { Some(goal) };
    // CLI flag overrides
    config.dry_run = Some(flags.dry_run);
    config.auto_approve = Some(flags.auto_approve);
    config.no_retro = Some(flags.no_retro);
    config.verbose = Some(flags.verbose);
    if flags.preserve_sandbox {
        config.sandbox.get_or_insert_default().preserve = Some(true);
    }
    config
}

/// Create a workflow run: allocate run directory, persist RunRecord, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub async fn create_run(
    args: &RunArgs,
    run_defaults: FabroConfig,
    styles: &Styles,
    quiet: bool,
) -> anyhow::Result<(String, PathBuf)> {
    let prep = prepare_workflow(args, run_defaults, styles, quiet)?;
    let dot_source = prep.source().to_string();
    let graph = prep.graph().clone();

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
    tokio::fs::write(cached_graph_path(&run_dir), &dot_source).await?;
    tokio::fs::write(run_dir.join("id.txt"), &run_id).await?;
    std::fs::File::create(run_dir.join("progress.jsonl"))?;
    fabro_workflows::run_status::write_run_status(
        &run_dir,
        fabro_workflows::run_status::RunStatus::Submitted,
        None,
    );

    // Copy the original workflow TOML into the run dir as a debug artifact.
    write_run_config_snapshot(&run_dir, prep.workflow_toml_path.as_deref()).await?;

    // Build normalized config and RunRecord
    let working_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let labels: std::collections::HashMap<String, String> = args
        .label
        .iter()
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let config = normalize_config(
        prep.run_cfg.as_ref(),
        &prep.run_defaults,
        &prep.model,
        prep.provider.as_deref(),
        prep.sandbox_provider,
        &graph,
        CliFlags::from(args),
    );

    let base_branch = fabro_sandbox::daytona::detect_repo_info(&working_directory)
        .ok()
        .and_then(|(_, branch)| branch);
    let record = RunRecord {
        run_id: run_id.clone(),
        created_at: Utc::now(),
        config,
        graph,
        workflow_slug: prep.workflow_slug.clone(),
        working_directory: working_directory.clone(),
        host_repo_path: Some(working_directory.to_string_lossy().to_string()),
        base_branch,
        labels,
    };
    record.save(&run_dir)?;

    Ok((run_id, run_dir))
}
