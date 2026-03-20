use std::path::PathBuf;

use anyhow::bail;
use chrono::Local;
use fabro_config::project as project_config;
use fabro_config::run::RunDefaults;
use fabro_validate::Severity;
use fabro_workflows::run_spec::RunSpec;
use fabro_workflows::sandbox_provider::SandboxProvider;
use fabro_workflows::workflow::WorkflowBuilder;

use super::run::{
    apply_goal_override, resolve_cli_goal, resolve_model_provider, resolve_sandbox_provider,
    RunArgs,
};
use super::shared::{print_diagnostics, read_workflow_file, relative_path};
use fabro_util::terminal::Styles;

/// Create a workflow run: allocate run directory, persist spec, return (run_id, run_dir).
///
/// This does NOT execute the workflow — it only prepares the run directory.
pub async fn create_run(
    args: &RunArgs,
    mut run_defaults: RunDefaults,
    styles: &Styles,
) -> anyhow::Result<(String, PathBuf)> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;

    // Apply project-level config overrides
    if let Ok(Some((_config_path, project_config))) =
        project_config::discover_project_config(&std::env::current_dir().unwrap_or_default())
    {
        tracing::debug!("Applying run defaults from fabro.toml");
        run_defaults.merge_overlay(project_config.into_run_defaults());
    }

    // Resolve workflow arg, load run config if TOML
    let (dot_path, run_cfg) = {
        let (dot, cfg) = project_config::resolve_workflow(workflow_path)?;
        match cfg {
            Some(mut cfg) => {
                cfg.apply_defaults(&run_defaults);
                (dot, Some(cfg))
            }
            None => (dot, None),
        }
    };

    let directory = run_cfg
        .as_ref()
        .and_then(|c| c.work_dir.as_deref())
        .or(run_defaults.work_dir.as_deref());
    if let Some(dir) = directory {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("Failed to set working directory to {dir}: {e}"))?;
    }

    // Parse and validate workflow
    let source = read_workflow_file(&dot_path)?;
    let vars = run_cfg
        .as_ref()
        .and_then(|c| c.vars.as_ref())
        .or(run_defaults.vars.as_ref());
    let source = match vars {
        Some(vars) => fabro_workflows::vars::expand_vars(&source, vars)?,
        None => source,
    };
    let dot_dir = dot_path.parent().unwrap_or(std::path::Path::new("."));
    let (mut graph, diagnostics) =
        WorkflowBuilder::new().prepare_with_file_inlining(&source, dot_dir)?;
    let cli_goal = resolve_cli_goal(&args.goal, &args.goal_file)?;
    let toml_goal = run_cfg.as_ref().and_then(|c| c.goal.as_deref());
    apply_goal_override(&mut graph, cli_goal.as_deref(), toml_goal);

    // Inline @file references in the goal
    if let Some(fabro_graphviz::graph::AttrValue::String(goal)) = graph.attrs.get("goal") {
        let fallback = dirs::home_dir().map(|h| h.join(".fabro"));
        let resolved =
            fabro_workflows::transform::resolve_file_ref(goal, dot_dir, fallback.as_deref());
        if resolved != *goal {
            graph.attrs.insert(
                "goal".to_string(),
                fabro_graphviz::graph::AttrValue::String(resolved),
            );
        }
    }

    eprintln!(
        "{} {} {}",
        styles.bold.apply_to("Workflow:"),
        graph.name,
        styles.dim.apply_to(format!(
            "({} nodes, {} edges)",
            graph.nodes.len(),
            graph.edges.len()
        )),
    );
    eprintln!(
        "{} {}",
        styles.dim.apply_to("Graph:"),
        styles.dim.apply_to(relative_path(&dot_path)),
    );

    let goal = graph.goal();
    if !goal.is_empty() {
        let first_line = goal.lines().next().unwrap_or(goal);
        eprintln!("{} {first_line}\n", styles.bold.apply_to("Goal:"));
    }

    print_diagnostics(&diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    // Resolve sandbox provider
    let sandbox_provider = if args.dry_run {
        SandboxProvider::Local
    } else {
        resolve_sandbox_provider(
            args.sandbox.map(Into::into),
            run_cfg.as_ref(),
            &run_defaults,
        )?
    };

    // Resolve model and provider
    let (model, provider) = resolve_model_provider(
        args.model.as_deref(),
        args.provider.as_deref(),
        run_cfg.as_ref(),
        &run_defaults,
        &graph,
    );

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
    tokio::fs::write(run_dir.join("graph.fabro"), &source).await?;
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
        dot_source: source,
        working_directory,
        goal: if goal.is_empty() {
            None
        } else {
            Some(goal.to_string())
        },
        model,
        provider,
        sandbox_provider: sandbox_provider.to_string(),
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
