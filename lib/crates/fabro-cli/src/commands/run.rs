use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{bail, Context};
use chrono::Local;
use clap::{Args, ValueEnum};
use fabro_agent::{
    DockerSandbox, DockerSandboxConfig, LocalSandbox, Sandbox, WorktreeConfig, WorktreeSandbox,
};
use fabro_config::config::FabroConfig;
use fabro_config::{project as project_config, run as run_config, sandbox as sandbox_config};
use fabro_interview::{AutoApproveInterviewer, ConsoleInterviewer, FileInterviewer, Interviewer};
use fabro_model::{Catalog, FallbackTarget, Provider};
use fabro_util::terminal::Styles;
use fabro_workflows::checkpoint::Checkpoint;
use fabro_workflows::cost::{compute_stage_cost, format_cost};
use fabro_workflows::devcontainer_bridge;
use fabro_workflows::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use fabro_workflows::git::GitSyncStatus;
use fabro_workflows::handler::default_registry;
use fabro_workflows::handler::llm::{AgentApiBackend, AgentCliBackend, BackendRouter};
use fabro_workflows::operations::{
    create_from_graph, start, StartFinalizeConfig, StartOptions, StartRetroConfig,
};
use fabro_workflows::outcome::StageStatus;
use fabro_workflows::pipeline::{
    build_conclusion, classify_engine_result, persist_terminal_outcome,
};
use fabro_workflows::run_settings::{GitCheckpointSettings, LifecycleConfig, RunSettings};
use fabro_workflows::sandbox_provider::SandboxProvider;
use indicatif::HumanDuration;
use std::time::Duration;
use tracing::debug;

use super::detached_support::{self, DetachedRunBootstrapGuard, DetachedRunCompletionGuard};
use super::run_progress;
use crate::commands::shared::{
    format_tokens_human, print_diagnostics, read_workflow_file, relative_path, tilde_path,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliSandboxProvider {
    Local,
    Docker,
    Daytona,
    #[cfg(feature = "exedev")]
    Exe,
    Ssh,
}

impl From<CliSandboxProvider> for SandboxProvider {
    fn from(value: CliSandboxProvider) -> Self {
        match value {
            CliSandboxProvider::Local => Self::Local,
            CliSandboxProvider::Docker => Self::Docker,
            CliSandboxProvider::Daytona => Self::Daytona,
            #[cfg(feature = "exedev")]
            CliSandboxProvider::Exe => Self::Exe,
            CliSandboxProvider::Ssh => Self::Ssh,
        }
    }
}

impl From<SandboxProvider> for CliSandboxProvider {
    fn from(value: SandboxProvider) -> Self {
        match value {
            SandboxProvider::Local => Self::Local,
            SandboxProvider::Docker => Self::Docker,
            SandboxProvider::Daytona => Self::Daytona,
            #[cfg(feature = "exedev")]
            SandboxProvider::Exe => Self::Exe,
            SandboxProvider::Ssh => Self::Ssh,
        }
    }
}

#[derive(Args)]
pub struct RunArgs {
    /// Path to a .fabro workflow file or .toml task config
    #[arg(required = true)]
    pub workflow: Option<PathBuf>,

    /// Run output directory
    #[arg(long)]
    pub run_dir: Option<PathBuf>,

    /// Execute with simulated LLM backend
    #[arg(long)]
    pub dry_run: bool,

    /// Validate run configuration without executing
    #[arg(long, conflicts_with = "dry_run")]
    pub preflight: bool,

    /// Auto-approve all human gates
    #[arg(long)]
    pub auto_approve: bool,

    /// Override the workflow goal (exposed as $goal in prompts)
    #[arg(long)]
    pub goal: Option<String>,

    /// Read the workflow goal from a file
    #[arg(long, conflicts_with = "goal")]
    pub goal_file: Option<PathBuf>,

    /// Override default LLM model
    #[arg(long)]
    pub model: Option<String>,

    /// Override default LLM provider
    #[arg(long)]
    pub provider: Option<String>,

    /// Enable verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Sandbox for agent tools
    #[arg(long, value_enum)]
    pub sandbox: Option<CliSandboxProvider>,

    /// Attach a label to this run (repeatable, format: KEY=VALUE)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub label: Vec<String>,

    /// Skip retro generation after the run
    #[arg(long)]
    pub no_retro: bool,

    /// Keep the sandbox alive after the run finishes (for debugging)
    #[arg(long)]
    pub preserve_sandbox: bool,

    /// Run the workflow in the background and print the run ID
    #[arg(short = 'd', long, conflicts_with = "preflight")]
    pub detach: bool,

    /// Pre-generated run ID (used internally by --detach)
    #[arg(long, hide = true)]
    pub run_id: Option<String>,
}

/// Resolve goal from `--goal` string or `--goal-file` path.
pub(crate) fn resolve_cli_goal(
    goal: &Option<String>,
    goal_file: &Option<PathBuf>,
) -> anyhow::Result<Option<String>> {
    match (goal, goal_file) {
        (Some(g), _) => Ok(Some(g.clone())),
        (_, Some(path)) => {
            let path = fabro_util::path::expand_tilde(path);
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read goal file: {}", path.display()))?;
            debug!(path = %path.display(), "Goal loaded from file");
            Ok(Some(content))
        }
        _ => Ok(None),
    }
}

/// Apply goal to the graph from TOML config or CLI flag.
/// Precedence: CLI `--goal` / `--goal-file` > TOML `goal` > DOT `graph [goal="..."]`.
pub(crate) fn apply_goal_override(
    graph: &mut fabro_graphviz::graph::Graph,
    cli_goal: Option<&str>,
    toml_goal: Option<&str>,
) {
    let goal = cli_goal.or(toml_goal);
    if let Some(goal) = goal {
        debug!(goal = %goal, "overriding graph goal");
        graph.attrs.insert(
            "goal".to_string(),
            fabro_graphviz::graph::AttrValue::String(goal.to_string()),
        );
    }
}

/// Compute the default run directory when `--run-dir` is not provided.
pub(crate) fn default_run_dir(run_id: &str, dry_run: bool) -> PathBuf {
    let base = fabro_workflows::run_lookup::default_runs_base();
    if dry_run {
        base.join(format!(
            "{}-dry-run-{}",
            Local::now().format("%Y%m%d"),
            run_id
        ))
    } else {
        base.join(format!("{}-{}", Local::now().format("%Y%m%d"), run_id))
    }
}

pub(crate) fn workflow_slug_from_path(workflow_path: &Path) -> Option<String> {
    let file_name = workflow_path.file_name()?.to_string_lossy();
    if workflow_path.extension().is_none() {
        return Some(file_name.into_owned());
    }

    let file_stem = workflow_path.file_stem()?.to_string_lossy();
    if file_stem == "workflow" {
        return workflow_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| Some(file_stem.into_owned()));
    }

    Some(file_stem.into_owned())
}

fn is_cached_run_restart(workflow_path: &Path, run_dir: &Path) -> bool {
    workflow_path.starts_with(run_dir)
        && workflow_path.file_name().is_some_and(|f| {
            f == std::ffi::OsStr::new(RUN_CONFIG_FILE) || f == std::ffi::OsStr::new(RUN_GRAPH_FILE)
        })
}

/// Resolve model and provider through the full precedence chain:
/// CLI flag > TOML config > run defaults > DOT graph attrs > provider-specific defaults.
/// Then resolve through the catalog for alias expansion.
pub(crate) fn resolve_model_provider(
    cli_model: Option<&str>,
    cli_provider: Option<&str>,
    run_cfg: Option<&FabroConfig>,
    run_defaults: &FabroConfig,
    graph: &fabro_graphviz::graph::Graph,
) -> (String, Option<String>) {
    let toml_model = run_cfg
        .and_then(|c| c.llm.as_ref())
        .and_then(|l| l.model.as_deref());
    let toml_provider = run_cfg
        .and_then(|c| c.llm.as_ref())
        .and_then(|l| l.provider.as_deref());
    let defaults_model = run_defaults.llm.as_ref().and_then(|l| l.model.as_deref());
    let defaults_provider = run_defaults
        .llm
        .as_ref()
        .and_then(|l| l.provider.as_deref());

    // Precedence: CLI flag > TOML > run defaults > DOT graph attrs > defaults
    let provider = cli_provider
        .or(toml_provider)
        .or(defaults_provider)
        .or_else(|| graph.attrs.get("default_provider").and_then(|v| v.as_str()))
        .map(String::from);

    let model = cli_model
        .or(toml_model)
        .or(defaults_model)
        .or_else(|| graph.attrs.get("default_model").and_then(|v| v.as_str()))
        .map(String::from)
        .unwrap_or_else(|| {
            let catalog = Catalog::builtin();
            let info = provider
                .as_deref()
                .and_then(|s| s.parse::<Provider>().ok())
                .and_then(|p| catalog.default_for_provider(p))
                .unwrap_or_else(|| catalog.default_from_env());
            info.id.clone()
        });

    // Resolve model alias through catalog
    match Catalog::builtin().get(&model) {
        Some(info) => (
            info.id.clone(),
            provider.or(Some(info.provider.to_string())),
        ),
        None => (model, provider),
    }
}

/// Parse sandbox provider from an optional `SandboxConfig`.
pub(crate) fn parse_sandbox_provider(
    sandbox: Option<&sandbox_config::SandboxConfig>,
) -> anyhow::Result<Option<SandboxProvider>> {
    sandbox
        .and_then(|s| s.provider.as_deref())
        .map(|s| s.parse::<SandboxProvider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("Invalid sandbox provider: {e}"))
}

/// Resolve sandbox provider: CLI flag > TOML config > run defaults > default.
pub(crate) fn resolve_sandbox_provider(
    cli: Option<SandboxProvider>,
    run_cfg: Option<&FabroConfig>,
    run_defaults: &FabroConfig,
) -> anyhow::Result<SandboxProvider> {
    let toml = parse_sandbox_provider(run_cfg.and_then(|c| c.sandbox.as_ref()))?;
    let defaults = parse_sandbox_provider(run_defaults.sandbox.as_ref())?;
    Ok(cli.or(toml).or(defaults).unwrap_or_default())
}

/// Resolve preserve-sandbox: CLI flag > TOML config > run defaults > false.
pub(crate) fn resolve_preserve_sandbox(
    cli: bool,
    run_cfg: Option<&FabroConfig>,
    run_defaults: &FabroConfig,
) -> bool {
    if cli {
        return true;
    }
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|s| s.preserve)
        .or_else(|| run_defaults.sandbox.as_ref().and_then(|s| s.preserve))
        .unwrap_or(false)
}

/// Resolve worktree mode: TOML config > run defaults > Clean.
fn resolve_worktree_mode(
    run_cfg: Option<&FabroConfig>,
    run_defaults: &FabroConfig,
) -> sandbox_config::WorktreeMode {
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|s| s.local.as_ref())
        .map(|l| l.worktree_mode)
        .unwrap_or_else(|| {
            run_defaults
                .sandbox
                .as_ref()
                .and_then(|s| s.local.as_ref())
                .map(|l| l.worktree_mode)
                .unwrap_or_default()
        })
}

/// Resolve daytona config: TOML config > run defaults.
pub(crate) fn resolve_daytona_config(
    run_cfg: Option<&FabroConfig>,
    run_defaults: &FabroConfig,
) -> Option<fabro_sandbox::daytona::DaytonaConfig> {
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|e| e.daytona.clone())
        .or_else(|| {
            run_defaults
                .sandbox
                .as_ref()
                .and_then(|s| s.daytona.clone())
        })
}

#[cfg(feature = "exedev")]
/// Resolve exe.dev config: TOML config > run defaults.
pub(crate) fn resolve_exe_config(
    run_cfg: Option<&FabroConfig>,
    run_defaults: &FabroConfig,
) -> Option<fabro_sandbox::exe::ExeConfig> {
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|e| e.exe.clone())
        .or_else(|| run_defaults.sandbox.as_ref().and_then(|s| s.exe.clone()))
}

#[cfg(feature = "exedev")]
/// Resolve exe.dev git clone parameters from the current repo.
///
/// Returns `None` if no git repo is detected. Credential resolution is
/// handled by ExeSandbox itself via its `github_app` field.
pub(crate) fn resolve_exe_clone_params(
    cwd: &std::path::Path,
) -> Option<fabro_sandbox::exe::GitCloneParams> {
    let (detected_url, branch) = match fabro_sandbox::daytona::detect_repo_info(cwd) {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("No git repo detected for exe.dev clone: {e}");
            return None;
        }
    };
    let url = fabro_github::ssh_url_to_https(&detected_url);
    Some(fabro_sandbox::exe::GitCloneParams { url, branch })
}

/// Resolve SSH sandbox config: TOML config > run defaults.
pub(crate) fn resolve_ssh_config(
    run_cfg: Option<&FabroConfig>,
    run_defaults: &FabroConfig,
) -> Option<fabro_sandbox::ssh::SshConfig> {
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|e| e.ssh.clone())
        .or_else(|| run_defaults.sandbox.as_ref().and_then(|s| s.ssh.clone()))
}

/// Resolve SSH sandbox git clone parameters from the current repo.
///
/// Returns `None` if no git repo is detected. Credential resolution is
/// handled by SshSandbox itself via its `github_app` field.
pub(crate) fn resolve_ssh_clone_params(
    cwd: &std::path::Path,
) -> Option<fabro_sandbox::ssh::GitCloneParams> {
    let (detected_url, branch) = match fabro_sandbox::daytona::detect_repo_info(cwd) {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("No git repo detected for SSH clone: {e}");
            return None;
        }
    };
    let url = fabro_github::ssh_url_to_https(&detected_url);
    Some(fabro_sandbox::ssh::GitCloneParams { url, branch })
}

/// Resolve the fallback chain from config.
///
/// `merge_overlay` must be called before this — it merges
/// `run_defaults.llm.fallbacks` into `run_cfg.llm.fallbacks` already.
pub(crate) fn resolve_fallback_chain(
    provider: Provider,
    model: &str,
    run_cfg: Option<&FabroConfig>,
) -> Vec<FallbackTarget> {
    let fallbacks = run_cfg
        .and_then(|c| c.llm.as_ref())
        .and_then(|l| l.fallbacks.as_ref());

    match fallbacks {
        Some(map) => Catalog::builtin().build_fallback_chain(provider, model, map),
        None => Vec::new(),
    }
}

/// Mint a GitHub App Installation Access Token with the given permissions.
///
/// Signs a JWT, resolves `owner/repo` from `origin_url`, and requests a
/// scoped token. Returns the token string on success.
pub(crate) async fn mint_github_token(
    creds: &fabro_github::GitHubAppCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> anyhow::Result<String> {
    let https_url = fabro_github::ssh_url_to_https(origin_url);
    let (owner, repo) =
        fabro_github::parse_github_owner_repo(&https_url).map_err(|e| anyhow::anyhow!("{e}"))?;
    let jwt = fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let client = reqwest::Client::new();
    let perms_json = serde_json::to_value(permissions)?;
    let token = fabro_github::create_installation_access_token_with_permissions(
        &client,
        &jwt,
        &owner,
        &repo,
        fabro_github::GITHUB_API_BASE_URL,
        perms_json,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(token)
}

/// How the workflow run's working directory is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkdirStrategy {
    /// Run directly in the current working directory.
    LocalDirectory,
    /// Create a local git worktree for isolation.
    LocalWorktree,
    /// Remote sandbox clones from origin (Daytona, Exe, SSH).
    Cloud,
}

/// Accumulates token usage and cost across all workflow stages.
#[derive(Default)]
pub(crate) struct CostAccumulator {
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub total_cache_write_tokens: i64,
    pub total_reasoning_tokens: i64,
    pub total_cost: f64,
    pub has_pricing: bool,
}

/// Create a [`LocalSandbox`] wired to emit [`WorkflowRunEvent::Sandbox`] events.
pub(crate) fn local_sandbox_with_callback(
    cwd: PathBuf,
    emitter: Arc<EventEmitter>,
) -> Arc<dyn Sandbox> {
    let mut env = LocalSandbox::new(cwd);
    env.set_event_callback(Arc::new(move |event| {
        emitter.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
    }));
    Arc::new(env)
}

pub(crate) const RUN_GRAPH_FILE: &str = "workflow.fabro";
pub(crate) const RUN_CONFIG_FILE: &str = "workflow.toml";

pub(crate) fn cached_graph_path(run_dir: &Path) -> PathBuf {
    run_dir.join(RUN_GRAPH_FILE)
}

pub(crate) fn cached_run_config_path(run_dir: &Path) -> PathBuf {
    run_dir.join(RUN_CONFIG_FILE)
}

/// Copy the original workflow TOML into the run directory as a debug artifact.
/// Nothing reads this programmatically — execution uses RunRecord.
pub(crate) async fn write_run_config_snapshot(
    run_dir: &Path,
    workflow_toml_path: Option<&Path>,
) -> anyhow::Result<()> {
    if let Some(toml_path) = workflow_toml_path {
        if toml_path.is_file() {
            tokio::fs::copy(toml_path, cached_run_config_path(run_dir))
                .await
                .context("Failed to copy workflow TOML to run directory")?;
        }
    }
    Ok(())
}

pub(crate) fn resolve_workflow_source(
    workflow_path: &Path,
) -> anyhow::Result<(PathBuf, PathBuf, Option<FabroConfig>)> {
    let path = project_config::resolve_workflow_arg(workflow_path)?;
    if path.extension().is_some_and(|ext| ext == "toml") {
        match run_config::load_run_config(&path) {
            Ok(cfg) => {
                let dot = run_config::resolve_graph_path(
                    &path,
                    cfg.graph.as_deref().unwrap_or("workflow.fabro"),
                );
                Ok((path, dot, Some(cfg)))
            }
            // Backward compatibility for detached runs created before run.toml existed.
            // Use path.exists() to distinguish a genuinely missing run.toml from one
            // that exists but has a broken internal reference (e.g. missing Dockerfile).
            Err(_)
                if !path.exists()
                    && path.starts_with(fabro_workflows::run_lookup::default_runs_base()) =>
            {
                Ok((path.clone(), path.with_file_name(RUN_GRAPH_FILE), None))
            }
            Err(err) => Err(err),
        }
    } else {
        Ok((path.clone(), path, None))
    }
}

/// Result of workflow preparation (shared between `create` and `run` commands).
pub(crate) struct PreparedWorkflow {
    pub validated: fabro_workflows::pipeline::Validated,
    pub raw_source: String,
    pub run_cfg: Option<FabroConfig>,
    pub sandbox_provider: SandboxProvider,
    pub model: String,
    pub provider: Option<String>,
    pub workflow_slug: Option<String>,
    pub run_defaults: FabroConfig,
    /// Resolved TOML path (Some for TOML-based workflows, None for bare .fabro).
    pub workflow_toml_path: Option<PathBuf>,
}

impl PreparedWorkflow {
    /// Read-through to validated graph.
    pub fn graph(&self) -> &fabro_graphviz::graph::Graph {
        self.validated.graph()
    }
    /// Original DOT source as authored on disk, before runtime var expansion.
    pub fn source(&self) -> &str {
        &self.raw_source
    }
}

/// Resolve config, parse/validate the workflow graph, and resolve sandbox + model.
///
/// Shared between `create_run` (which only persists the spec) and
/// `run_command` (which goes on to execute the workflow).
pub(crate) fn prepare_workflow(
    args: &RunArgs,
    run_defaults: FabroConfig,
    styles: &Styles,
    quiet: bool,
) -> anyhow::Result<PreparedWorkflow> {
    prepare_workflow_with_project_config(args, run_defaults, styles, quiet, true)
}

pub(crate) fn prepare_workflow_with_project_config(
    args: &RunArgs,
    mut run_defaults: FabroConfig,
    styles: &Styles,
    quiet: bool,
    apply_project_config: bool,
) -> anyhow::Result<PreparedWorkflow> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;

    if apply_project_config {
        // Apply project-level config overrides (fabro.toml) on top of CLI defaults.
        if let Ok(Some((_config_path, project_config))) =
            project_config::discover_project_config(&std::env::current_dir().unwrap_or_default())
        {
            tracing::debug!("Applying run defaults from fabro.toml");
            run_defaults.merge_overlay(project_config);
        }
    }

    // Resolve workflow arg, load run config if TOML, merge with defaults
    let (resolved_workflow_path, dot_path, run_cfg) = {
        let (resolved, dot, cfg) = resolve_workflow_source(workflow_path)?;
        match cfg {
            Some(cfg) => {
                // run_defaults is the base; cfg (from workflow.toml) is the overlay that wins
                let mut merged = run_defaults.clone();
                merged.merge_overlay(cfg);
                (resolved, dot, Some(merged))
            }
            None => (resolved, dot, None),
        }
    };
    let workflow_slug = workflow_slug_from_path(&resolved_workflow_path);

    let directory = run_cfg
        .as_ref()
        .and_then(|c| c.work_dir.as_deref())
        .or(run_defaults.work_dir.as_deref());
    if let Some(dir) = directory {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("Failed to set working directory to {dir}: {e}"))?;
    }

    // Parse and transform workflow using pipeline functions
    let raw_source = read_workflow_file(&dot_path)?;
    let vars = run_cfg
        .as_ref()
        .and_then(|c| c.vars.as_ref())
        .or(run_defaults.vars.as_ref());
    let source = match vars {
        Some(vars) => fabro_workflows::vars::expand_vars(&raw_source, vars)?,
        None => raw_source.clone(),
    };
    let dot_dir = dot_path.parent().unwrap_or(std::path::Path::new("."));

    let parsed = fabro_workflows::pipeline::parse(&source)?;
    let mut transformed = fabro_workflows::pipeline::transform(
        parsed,
        &fabro_workflows::pipeline::TransformOptions {
            base_dir: Some(dot_dir.to_path_buf()),
            custom_transforms: vec![],
        },
    );

    // Apply goal override on the mutable transformed graph
    let cli_goal = resolve_cli_goal(&args.goal, &args.goal_file)?;
    let toml_goal = run_cfg.as_ref().and_then(|c| c.goal.as_deref());
    apply_goal_override(&mut transformed.graph, cli_goal.as_deref(), toml_goal);

    // Inline @file references in the (possibly overridden) goal
    if let Some(fabro_graphviz::graph::AttrValue::String(goal)) =
        transformed.graph.attrs.get("goal")
    {
        let fallback = dirs::home_dir().map(|h| h.join(".fabro"));
        let resolved =
            fabro_workflows::transform::resolve_file_ref(goal, dot_dir, fallback.as_deref());
        if resolved != *goal {
            transformed.graph.attrs.insert(
                "goal".to_string(),
                fabro_graphviz::graph::AttrValue::String(resolved),
            );
        }
    }

    let validated = fabro_workflows::pipeline::validate(transformed, &[]);

    if !quiet {
        eprintln!(
            "{} {} {}",
            styles.bold.apply_to("Workflow:"),
            validated.graph().name,
            styles.dim.apply_to(format!(
                "({} nodes, {} edges)",
                validated.graph().nodes.len(),
                validated.graph().edges.len()
            )),
        );
        eprintln!(
            "{} {}",
            styles.dim.apply_to("Graph:"),
            styles.dim.apply_to(relative_path(&dot_path)),
        );

        let goal = validated.graph().goal();
        if !goal.is_empty() {
            let stripped = fabro_util::text::strip_goal_decoration(goal);
            eprintln!("{} {stripped}\n", styles.bold.apply_to("Goal:"));
        }

        print_diagnostics(validated.diagnostics(), styles);
    }

    if validated.has_errors() {
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
        validated.graph(),
    );

    let workflow_toml_path = if resolved_workflow_path
        .extension()
        .is_some_and(|ext| ext == "toml")
    {
        Some(resolved_workflow_path)
    } else {
        None
    };

    Ok(PreparedWorkflow {
        validated,
        raw_source,
        run_cfg,
        sandbox_provider,
        model,
        provider,
        workflow_slug,
        run_defaults,
        workflow_toml_path,
    })
}

/// Pre-prepared run state, used to skip workflow preparation in `run_command_impl`.
struct RecordBasedRun {
    validated: fabro_workflows::pipeline::Validated,
    raw_source: String,
    run_cfg: Option<FabroConfig>,
    sandbox_provider: SandboxProvider,
    model: String,
    provider: Option<String>,
    workflow_slug: Option<String>,
    run_defaults: FabroConfig,
    /// Original TOML path for debug snapshot (None for record-based or bare .fabro runs).
    workflow_toml_path: Option<PathBuf>,
}

/// Execute a workflow run from a saved RunRecord, bypassing workflow preparation.
///
/// Used by `run_engine_entrypoint` for detached runs that already have a RunRecord on disk.
pub async fn run_from_record(
    record: fabro_workflows::run_record::RunRecord,
    run_dir: PathBuf,
    run_defaults: FabroConfig,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<()> {
    let sandbox_provider = record
        .config
        .sandbox
        .as_ref()
        .and_then(|s| s.provider.as_deref())
        .unwrap_or("local")
        .parse()
        .unwrap_or(SandboxProvider::Local);
    let model = record
        .config
        .llm
        .as_ref()
        .and_then(|l| l.model.clone())
        .unwrap_or_default();
    let provider = record
        .config
        .llm
        .as_ref()
        .and_then(|l| l.provider.clone())
        .filter(|s| !s.is_empty());

    let record_run = RecordBasedRun {
        raw_source: String::new(), // Raw DOT provenance is best-effort for record-based runs
        validated: create_from_graph(record.graph.clone(), String::new()),
        run_cfg: Some(record.config.clone()),
        sandbox_provider,
        model: model.clone(),
        provider: provider.clone(),
        workflow_slug: record.workflow_slug.clone(),
        run_defaults,
        workflow_toml_path: None, // No TOML to copy — config is in RunRecord
    };

    let args = RunArgs {
        workflow: None,
        run_dir: Some(run_dir),
        dry_run: record.config.dry_run_enabled(),
        preflight: false,
        auto_approve: record.config.auto_approve_enabled(),
        goal: record.config.goal.clone(),
        goal_file: None,
        model: Some(model),
        provider,
        verbose: record.config.verbose_enabled(),
        sandbox: Some(CliSandboxProvider::from(sandbox_provider)),
        label: record
            .labels
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect(),
        no_retro: record.config.no_retro_enabled(),
        preserve_sandbox: record
            .config
            .sandbox
            .as_ref()
            .and_then(|s| s.preserve)
            .unwrap_or(false),
        detach: false,
        run_id: Some(record.run_id),
    };

    run_command_impl(args, styles, github_app, git_author, Some(record_run)).await
}

/// Execute a full workflow run.
///
/// # Errors
///
/// Returns an error if the workflow cannot be read, parsed, validated, or executed.
pub async fn run_command(
    args: RunArgs,
    run_defaults: FabroConfig,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<()> {
    let PreparedWorkflow {
        validated,
        raw_source,
        run_cfg,
        sandbox_provider,
        model,
        provider,
        workflow_slug,
        run_defaults,
        workflow_toml_path,
    } = prepare_workflow(&args, run_defaults, styles, false)?;

    let record_run = RecordBasedRun {
        validated,
        raw_source,
        run_cfg,
        sandbox_provider,
        model,
        provider,
        workflow_slug,
        run_defaults,
        workflow_toml_path,
    };

    run_command_impl(args, styles, github_app, git_author, Some(record_run)).await
}

async fn run_command_impl(
    args: RunArgs,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
    record_run: Option<RecordBasedRun>,
) -> anyhow::Result<()> {
    let (
        validated,
        raw_source,
        mut run_cfg,
        sandbox_provider,
        model,
        provider,
        prepared_workflow_slug,
        run_defaults,
        workflow_toml_path,
    ) = match record_run {
        Some(rr) => (
            rr.validated,
            rr.raw_source,
            rr.run_cfg,
            rr.sandbox_provider,
            rr.model,
            rr.provider,
            rr.workflow_slug,
            rr.run_defaults,
            rr.workflow_toml_path,
        ),
        None => unreachable!("run_command_impl always receives a RecordBasedRun"),
    };
    let graph = validated.graph().clone();

    // For record-based runs from run_from_record, workflow is None (preparation was skipped).
    let from_record = args.workflow.is_none();

    // Collect setup commands — they'll be run inside the sandbox
    let setup_commands: Vec<String> = run_cfg
        .as_ref()
        .and_then(|c| c.setup.as_ref())
        .or(run_defaults.setup.as_ref())
        .map(|s| s.commands.clone())
        .unwrap_or_default();

    // Pre-flight: check git cleanliness before creating any files
    let preserve_sandbox =
        resolve_preserve_sandbox(args.preserve_sandbox, run_cfg.as_ref(), &run_defaults);
    let original_cwd = std::env::current_dir()?;
    let (origin_url, detected_base_branch) =
        fabro_sandbox::daytona::detect_repo_info(&original_cwd)
            .map(|(url, branch)| (Some(url), branch))
            .unwrap_or((None, None));
    let git_status =
        fabro_workflows::git::sync_status(&original_cwd, "origin", detected_base_branch.as_deref());

    if args.preflight {
        return run_preflight(
            &graph,
            &run_cfg,
            &args,
            &run_defaults,
            git_status,
            sandbox_provider,
            styles,
            github_app,
            origin_url.as_deref(),
        )
        .await;
    }

    // 3. Create logs directory
    // Extract values from args before partial move
    let dry_run_flag = args.dry_run;
    let auto_approve_flag = args.auto_approve;
    let no_retro_flag = args.no_retro;
    let verbose_flag = args.verbose;
    let preserve_sandbox_flag = args.preserve_sandbox;
    let label_vec = args.label.clone();
    let run_id = args.run_id.unwrap_or_else(|| ulid::Ulid::new().to_string());
    let run_dir = args
        .run_dir
        .unwrap_or_else(|| default_run_dir(&run_id, dry_run_flag));
    tokio::fs::create_dir_all(&run_dir).await?;
    let cached_run_restart = if from_record {
        // Record-based runs already have RunRecord on disk — skip re-writing.
        true
    } else {
        let workflow_path = args.workflow.as_ref().unwrap();
        is_cached_run_restart(workflow_path, &run_dir)
    };
    let existing_record = if cached_run_restart {
        fabro_workflows::run_record::RunRecord::load(&run_dir).ok()
    } else {
        None
    };
    let workflow_slug = if cached_run_restart {
        existing_record
            .as_ref()
            .and_then(|r| r.workflow_slug.clone())
    } else {
        prepared_workflow_slug
    };
    fabro_util::run_log::activate(&run_dir.join("cli.log"))
        .context("Failed to activate per-run log")?;
    if !from_record && !raw_source.is_empty() {
        tokio::fs::write(cached_graph_path(&run_dir), &raw_source).await?;
    }
    let mut status_guard = DetachedRunBootstrapGuard::arm(&run_dir)?;

    // Copy the original workflow TOML as a debug artifact.
    // Skip for record-based runs (already exists) and cached restarts.
    if !from_record && !cached_run_restart {
        write_run_config_snapshot(&run_dir, workflow_toml_path.as_deref()).await?;
    }

    // Write RunRecord and compute normalized config for RunSettings
    let settings_config = if !cached_run_restart {
        let cli_flags = super::create::CliFlags {
            dry_run: dry_run_flag,
            auto_approve: auto_approve_flag,
            no_retro: no_retro_flag,
            verbose: verbose_flag,
            preserve_sandbox: preserve_sandbox_flag,
        };
        let normalized_config = super::create::normalize_config(
            run_cfg.as_ref(),
            &run_defaults,
            &model,
            provider.as_deref(),
            sandbox_provider,
            &graph,
            cli_flags,
        );
        let record = fabro_workflows::run_record::RunRecord {
            run_id: run_id.clone(),
            created_at: chrono::Utc::now(),
            config: normalized_config.clone(),
            graph: graph.clone(),
            workflow_slug: workflow_slug.clone(),
            working_directory: original_cwd.clone(),
            host_repo_path: Some(original_cwd.to_string_lossy().to_string()),
            base_branch: detected_base_branch.clone(),
            labels: label_vec
                .iter()
                .filter_map(|s| s.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        record.save(&run_dir)?;
        normalized_config
    } else {
        existing_record
            .as_ref()
            .map(|r| r.config.clone())
            .unwrap_or_default()
    };

    // Now resolve ${env.VARNAME} references for runtime use.
    if let Some(ref mut cfg) = run_cfg {
        run_config::resolve_sandbox_env(cfg)?;
    }

    // Create progress UI (used for both normal and verbose modes)
    let is_tty = std::io::stderr().is_terminal();
    let progress_ui = Arc::new(Mutex::new(run_progress::ProgressUI::new(
        is_tty,
        verbose_flag,
    )));
    {
        let mut ui = progress_ui.lock().expect("progress lock poisoned");
        ui.show_version();
        ui.show_run_id(&run_id);
        ui.show_time(&Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
        ui.show_run_dir(&run_dir);
    }

    // 3. Build event emitter
    let emitter = EventEmitter::new();

    // Cost accumulator — shared across all verbosity levels
    let accumulator = Arc::new(Mutex::new(CostAccumulator::default()));
    let acc_clone = Arc::clone(&accumulator);
    emitter.on_event(move |event| {
        if let fabro_workflows::event::WorkflowRunEvent::StageCompleted { usage: Some(u), .. } =
            event
        {
            let mut acc = acc_clone.lock().unwrap();
            acc.total_input_tokens += u.input_tokens;
            acc.total_output_tokens += u.output_tokens;
            acc.total_cache_read_tokens += u.cache_read_tokens.unwrap_or(0);
            acc.total_cache_write_tokens += u.cache_write_tokens.unwrap_or(0);
            acc.total_reasoning_tokens += u.reasoning_tokens.unwrap_or(0);
            if let Some(cost) = compute_stage_cost(u) {
                acc.total_cost += cost;
                acc.has_pricing = true;
            }
        }
    });

    // JSONL progress log + live.json snapshot
    {
        let jsonl_path = run_dir.join("progress.jsonl");
        let live_path = run_dir.join("live.json");
        let run_id = Arc::new(Mutex::new(run_id.clone()));
        let run_id_clone = Arc::clone(&run_id);
        emitter.on_event(move |event| {
            if let fabro_workflows::event::WorkflowRunEvent::WorkflowRunStarted { run_id, .. } =
                event
            {
                *run_id_clone.lock().unwrap() = run_id.clone();
            }
            let envelope = build_event_envelope(event, &run_id_clone.lock().unwrap());
            // Append to progress.jsonl
            if let Ok(line) = serde_json::to_string(&envelope) {
                let line = fabro_util::redact::redact_jsonl_line(&line);
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&jsonl_path)
                {
                    let _ = writeln!(f, "{line}");
                }
            }
            // Overwrite live.json
            if let Ok(pretty) = serde_json::to_string_pretty(&envelope) {
                let pretty = fabro_util::redact::redact_jsonl_line(&pretty);
                let _ = std::fs::write(&live_path, pretty);
            }
        });
    }

    run_progress::ProgressUI::register(&progress_ui, &emitter);

    // 4. Build interviewer
    let interviewer: Arc<dyn Interviewer> = if auto_approve_flag {
        Arc::new(AutoApproveInterviewer)
    } else if !std::io::stdin().is_terminal() {
        // Detached mode (stdin is /dev/null): use file-based IPC so the
        // attach process can prompt the user on our behalf.
        Arc::new(FileInterviewer::new(run_dir.clone()))
    } else {
        Arc::new(run_progress::ProgressAwareInterviewer::new(
            ConsoleInterviewer::new(styles),
            Arc::clone(&progress_ui),
        ))
    };

    // Determine the working directory strategy.
    // Only the Local provider supports git worktrees on the host.
    // Remote sandboxes (Daytona, Exe, SSH) clone from origin inside the sandbox.
    // Docker uses the bind-mounted host directory as-is.
    let workdir_strategy = match sandbox_provider {
        SandboxProvider::Local => {
            let worktree_mode = resolve_worktree_mode(run_cfg.as_ref(), &run_defaults);
            match worktree_mode {
                sandbox_config::WorktreeMode::Always => WorkdirStrategy::LocalWorktree,
                sandbox_config::WorktreeMode::Clean => {
                    if git_status.is_clean() {
                        WorkdirStrategy::LocalWorktree
                    } else {
                        WorkdirStrategy::LocalDirectory
                    }
                }
                sandbox_config::WorktreeMode::Dirty => {
                    if git_status.is_clean() {
                        WorkdirStrategy::LocalDirectory
                    } else {
                        WorkdirStrategy::LocalWorktree
                    }
                }
                sandbox_config::WorktreeMode::Never => WorkdirStrategy::LocalDirectory,
            }
        }
        SandboxProvider::Docker => WorkdirStrategy::LocalDirectory,
        _ => WorkdirStrategy::Cloud,
    };
    debug!(
        ?workdir_strategy,
        ?sandbox_provider,
        ?git_status,
        "Resolved workdir strategy"
    );

    // Warn about uncommitted changes that won't be available in the execution environment.
    if git_status == GitSyncStatus::Dirty {
        let env_name = match workdir_strategy {
            WorkdirStrategy::LocalWorktree => Some("worktree"),
            WorkdirStrategy::Cloud => Some("remote sandbox"),
            WorkdirStrategy::LocalDirectory => None,
        };
        if let Some(env_name) = env_name {
            emit_run_notice(
                &emitter,
                RunNoticeLevel::Warn,
                "dirty_worktree",
                format!("Uncommitted changes will not be included in the {env_name}."),
            );
        }
    }

    // Auto-push when the execution environment needs commits on the remote.
    if !dry_run_flag
        && matches!(
            workdir_strategy,
            WorkdirStrategy::LocalWorktree | WorkdirStrategy::Cloud
        )
    {
        if let Some(ref branch) = detected_base_branch {
            // For Synced we know no push is needed; for Unsynced we know it is;
            // for Dirty the push status wasn't checked, so check now.
            let needs_push = match git_status {
                GitSyncStatus::Synced => false,
                GitSyncStatus::Unsynced => true,
                GitSyncStatus::Dirty => {
                    let check_repo = original_cwd.clone();
                    let check_branch = branch.clone();
                    tokio::task::spawn_blocking(move || {
                        fabro_workflows::git::branch_needs_push(
                            &check_repo,
                            "origin",
                            &check_branch,
                        )
                    })
                    .await
                    .unwrap_or(true)
                }
            };

            if needs_push {
                let repo_path = original_cwd.clone();
                let branch_owned = branch.clone();
                let result = fabro_workflows::git::blocking_push_with_timeout(60, move || {
                    fabro_workflows::git::push_branch(&repo_path, "origin", &branch_owned)
                })
                .await;
                match result {
                    Ok(()) => {
                        tracing::info!(%branch, "Pushed current branch to origin");
                        emit_run_notice(
                            &emitter,
                            RunNoticeLevel::Info,
                            "git_push_succeeded",
                            format!("{branch} (synced local commits to remote)"),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %branch, "Failed to push current branch");
                        emit_run_notice(
                            &emitter,
                            RunNoticeLevel::Warn,
                            "git_push_failed",
                            format!("Failed to push {branch} to origin: {e}"),
                        );
                    }
                }
            } else {
                tracing::info!(%branch, "Branch already in sync with origin, skipping push");
            }
        }
    }

    // Compute worktree configuration for local isolation.
    // The actual git setup (branch, worktree add, reset) happens inside the sandbox
    // creation block for SandboxProvider::Local below.
    let (mut worktree_path, mut worktree_branch, mut worktree_base_sha) = if workdir_strategy
        == WorkdirStrategy::LocalWorktree
    {
        match fabro_workflows::git::head_sha(&original_cwd) {
            Ok(base_sha) => {
                let branch_name = format!("{}{run_id}", fabro_workflows::git::RUN_BRANCH_PREFIX);
                let wt_path = run_dir.join("worktree");
                (Some(wt_path), Some(branch_name), Some(base_sha))
            }
            Err(e) => {
                emit_run_notice(
                    &emitter,
                    RunNoticeLevel::Warn,
                    "worktree_setup_failed",
                    format!("Git worktree setup failed ({e}), running without worktree."),
                );
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };

    if let Some(ref wt) = worktree_path {
        progress_ui
            .lock()
            .expect("progress lock poisoned")
            .show_worktree(wt);
    }

    // Show base SHA for both worktree and cloud strategies.
    let base_sha_display = worktree_base_sha.clone().or_else(|| {
        if workdir_strategy == WorkdirStrategy::Cloud {
            fabro_workflows::git::head_sha(&original_cwd).ok()
        } else {
            None
        }
    });
    if let Some(ref sha) = base_sha_display {
        progress_ui
            .lock()
            .expect("progress lock poisoned")
            .show_base_info(detected_base_branch.as_deref(), sha);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut daytona_config = resolve_daytona_config(run_cfg.as_ref(), &run_defaults);
    #[cfg(feature = "exedev")]
    let exe_config = resolve_exe_config(run_cfg.as_ref(), &run_defaults);
    let ssh_config = resolve_ssh_config(run_cfg.as_ref(), &run_defaults);

    // Resolve devcontainer if enabled
    let devcontainer_config = if run_cfg
        .as_ref()
        .and_then(|c| c.sandbox.as_ref())
        .or(run_defaults.sandbox.as_ref())
        .and_then(|s| s.devcontainer)
        .unwrap_or(false)
    {
        match fabro_devcontainer::DevcontainerResolver::resolve(&cwd).await {
            Ok(dc) => {
                let lifecycle_command_count = dc.on_create_commands.len()
                    + dc.post_create_commands.len()
                    + dc.post_start_commands.len();
                emitter.emit(
                    &fabro_workflows::event::WorkflowRunEvent::DevcontainerResolved {
                        dockerfile_lines: dc.dockerfile.lines().count(),
                        environment_count: dc.environment.len(),
                        lifecycle_command_count,
                        workspace_folder: dc.workspace_folder.clone(),
                    },
                );

                // Override daytona_config with devcontainer dockerfile
                let snapshot = devcontainer_bridge::devcontainer_to_snapshot_config(&dc);
                let mut cfg = daytona_config.unwrap_or_default();
                cfg.snapshot = Some(snapshot);
                daytona_config = Some(cfg);

                // Run initialize_commands on host
                let timeout = std::time::Duration::from_millis(300_000);
                for cmd in &dc.initialize_commands {
                    let shell_cmds = match cmd {
                        fabro_devcontainer::Command::Shell(s) => vec![s.clone()],
                        fabro_devcontainer::Command::Args(args) => {
                            vec![args
                                .iter()
                                .map(|a| {
                                    shlex::try_quote(a).unwrap_or_else(|_| a.into()).to_string()
                                })
                                .collect::<Vec<_>>()
                                .join(" ")]
                        }
                        fabro_devcontainer::Command::Parallel(map) => {
                            map.values().cloned().collect()
                        }
                    };
                    for shell_cmd in &shell_cmds {
                        let fut = tokio::process::Command::new("sh")
                            .arg("-c")
                            .arg(shell_cmd)
                            .current_dir(&cwd)
                            .output();
                        let output = tokio::time::timeout(timeout, fut)
                            .await
                            .with_context(|| {
                                format!("Devcontainer initializeCommand timed out: {shell_cmd}")
                            })?
                            .with_context(|| {
                                format!(
                                    "Failed to execute devcontainer initializeCommand: {shell_cmd}"
                                )
                            })?;
                        if !output.status.success() {
                            let code = output
                                .status
                                .code()
                                .map_or("unknown".to_string(), |c| c.to_string());
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            bail!(
                                "Devcontainer initializeCommand failed (exit code {code}): {shell_cmd}\n{stderr}"
                            );
                        }
                    }
                }

                Some(dc)
            }
            Err(e) => {
                bail!("Failed to resolve devcontainer: {e}");
            }
        }
    } else {
        None
    };

    // Deferred sandbox reference — filled after sandbox creation, consumed by event listeners.
    let deferred_sandbox: Arc<Mutex<Option<Arc<dyn Sandbox>>>> = Arc::new(Mutex::new(None));

    // Register SandboxInitialized listener (must happen before emitter is wrapped in Arc)
    {
        let run_dir_for_listener = run_dir.clone();
        let progress_for_listener = Arc::clone(&progress_ui);
        let cwd_for_listener = cwd.to_string_lossy().to_string();
        let ssh_data_host = ssh_config.as_ref().map(|c| c.destination.clone());
        let deferred_sb = Arc::clone(&deferred_sandbox);
        let provider = sandbox_provider; // Copy — captured by move closure
        emitter.on_event(move |event| {
            if let fabro_workflows::event::WorkflowRunEvent::SandboxInitialized {
                working_directory,
            } = event
            {
                progress_for_listener
                    .lock()
                    .expect("progress lock poisoned")
                    .set_working_directory(working_directory.clone());

                // Build sandbox record from template
                let sandbox_info_opt = deferred_sb.lock().unwrap().as_ref().and_then(|sb| {
                    let info = sb.sandbox_info();
                    if info.is_empty() {
                        None
                    } else {
                        Some(info)
                    }
                });

                let is_docker = provider == SandboxProvider::Docker;
                let record = fabro_workflows::sandbox_record::SandboxRecord {
                    provider: provider.to_string(),
                    working_directory: working_directory.clone(),
                    identifier: sandbox_info_opt,
                    host_working_directory: if is_docker {
                        Some(cwd_for_listener.clone())
                    } else {
                        None
                    },
                    container_mount_point: if is_docker {
                        Some(working_directory.clone())
                    } else {
                        None
                    },
                    data_host: if provider == SandboxProvider::Ssh {
                        ssh_data_host.clone()
                    } else {
                        None
                    },
                };
                if let Err(e) = record.save(&run_dir_for_listener.join("sandbox.json")) {
                    tracing::warn!(error = %e, "Failed to save sandbox record");
                }
            }
        });
    }

    // Wrap emitter in Arc so we can share it with exec env callbacks
    let emitter = Arc::new(emitter);

    let sandbox: Arc<dyn Sandbox> = match sandbox_provider {
        SandboxProvider::Docker => {
            let config = DockerSandboxConfig {
                host_working_directory: cwd.to_string_lossy().to_string(),
                ..DockerSandboxConfig::default()
            };
            let mut env = DockerSandbox::new(config)
                .map_err(|e| anyhow::anyhow!("Failed to create Docker environment: {e}"))?;
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        SandboxProvider::Daytona => {
            let config = daytona_config.clone().unwrap_or_default();
            let mut env = fabro_sandbox::daytona::DaytonaSandbox::new(
                config,
                github_app.clone(),
                Some(run_id.clone()),
                detected_base_branch.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        #[cfg(feature = "exedev")]
        SandboxProvider::Exe => {
            let clone_params = resolve_exe_clone_params(&original_cwd);

            let mgmt_ssh = fabro_sandbox::exe::OpensshRunner::connect_raw("exe.dev")
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to exe.dev: {e}"))?;
            let config = exe_config.unwrap_or_default();
            let mut env = fabro_sandbox::exe::ExeSandbox::new(
                Box::new(mgmt_ssh),
                config,
                clone_params,
                Some(run_id.clone()),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        SandboxProvider::Ssh => {
            let config = ssh_config
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--sandbox ssh requires [sandbox.ssh] config"))?;
            let clone_params = resolve_ssh_clone_params(&original_cwd);
            let mut env = fabro_sandbox::ssh::SshSandbox::new(
                config,
                clone_params,
                Some(run_id.clone()),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        SandboxProvider::Local => {
            if let (Some(base_sha), Some(branch_name), Some(wt_path)) = (
                worktree_base_sha.as_ref(),
                worktree_branch.as_ref(),
                worktree_path.as_ref(),
            ) {
                // Set up a WorktreeSandbox for git-isolated local execution.
                let wt_path_str = wt_path.to_string_lossy().into_owned();
                let inner = local_sandbox_with_callback(original_cwd.clone(), Arc::clone(&emitter));
                let wt_config = WorktreeConfig {
                    branch_name: branch_name.clone(),
                    base_sha: base_sha.clone(),
                    worktree_path: wt_path_str,
                    skip_branch_creation: false,
                };
                let mut wt_sandbox = WorktreeSandbox::new(inner, wt_config);
                wt_sandbox.set_event_callback(Arc::clone(&emitter).worktree_callback());

                match wt_sandbox.initialize().await {
                    Ok(()) => {
                        std::env::set_current_dir(wt_path)?;
                        Arc::new(wt_sandbox) as Arc<dyn Sandbox>
                    }
                    Err(e) => {
                        emit_run_notice(
                            &emitter,
                            RunNoticeLevel::Warn,
                            "worktree_setup_failed",
                            format!("Git worktree setup failed ({e}), running without worktree."),
                        );
                        // Reset so RunSettings does not enable git checkpointing
                        worktree_path = None;
                        worktree_branch = None;
                        worktree_base_sha = None;
                        local_sandbox_with_callback(cwd.clone(), Arc::clone(&emitter))
                    }
                }
            } else {
                local_sandbox_with_callback(cwd.clone(), Arc::clone(&emitter))
            }
        }
    };

    // Wrap with ReadBeforeWriteSandbox to enforce read-before-write guard
    // (delegate_sandbox! macro delegates initialize/cleanup)
    let sandbox: Arc<dyn Sandbox> = Arc::new(fabro_agent::ReadBeforeWriteSandbox::new(sandbox));

    // Fill deferred sandbox reference for event listeners registered above
    *deferred_sandbox.lock().unwrap() = Some(Arc::clone(&sandbox));

    // 6. Resolve backend, model, and provider
    let (dry_run_mode, llm_client) = if dry_run_flag {
        (true, None)
    } else {
        match fabro_llm::client::Client::from_env().await {
            Ok(c) if c.provider_names().is_empty() => {
                emit_run_notice(
                    &emitter,
                    RunNoticeLevel::Warn,
                    "dry_run_no_llm",
                    "No LLM providers configured. Running in dry-run mode.",
                );
                (true, None)
            }
            Ok(c) => (false, Some(c)),
            Err(e) => {
                emit_run_notice(
                    &emitter,
                    RunNoticeLevel::Warn,
                    "dry_run_llm_init_failed",
                    format!("Failed to initialize LLM client: {e}. Running in dry-run mode."),
                );
                (true, None)
            }
        }
    };

    // Parse provider string to enum (defaults to best available from env)
    let provider_enum: Provider = provider
        .as_deref()
        .map(|s| s.parse::<Provider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or_else(Provider::default_from_env);

    // Resolve fallback chain from config
    let fallback_chain = resolve_fallback_chain(provider_enum, &model, run_cfg.as_ref());

    // 7. Build engine
    // Devcontainer env is layered underneath TOML env (TOML wins on conflict)
    let sandbox_env: HashMap<String, String> = {
        let mut env = if let Some(ref dc) = devcontainer_config {
            dc.environment.clone()
        } else {
            HashMap::new()
        };
        if let Some(mut toml_env) = run_cfg
            .as_ref()
            .and_then(|c| c.sandbox.as_ref())
            .or(run_defaults.sandbox.as_ref())
            .and_then(|s| s.env.clone())
        {
            // When falling back to run_defaults (run_cfg is None, i.e. bare .fabro
            // workflow), env refs haven't been resolved yet — resolve them now.
            if run_cfg.is_none() {
                run_config::resolve_env_refs(&mut toml_env)?;
            }
            env.extend(toml_env);
        }
        env
    };

    // Mint a GitHub App IAT and inject as GITHUB_TOKEN if [github] permissions are declared
    let mut sandbox_env = sandbox_env;
    let github_permissions = run_cfg
        .as_ref()
        .and_then(|c| c.github.as_ref())
        .or(run_defaults.github.as_ref());
    if let Some(gh_cfg) = github_permissions {
        if !gh_cfg.permissions.is_empty() {
            if let (Some(ref creds), Some(ref url)) = (&github_app, &origin_url) {
                match mint_github_token(creds, url, &gh_cfg.permissions).await {
                    Ok(token) => {
                        debug!("Minted GitHub IAT for sandbox GITHUB_TOKEN");
                        sandbox_env.insert("GITHUB_TOKEN".to_string(), token);
                    }
                    Err(e) => {
                        emit_run_notice(
                            &emitter,
                            RunNoticeLevel::Warn,
                            "github_token_failed",
                            format!("Failed to mint GitHub token: {e}"),
                        );
                    }
                }
            } else {
                debug!("Skipping GitHub token: no GitHub App credentials or origin URL");
            }
        }
    }

    let mcp_servers: Vec<fabro_mcp::config::McpServerConfig> = {
        let servers = run_cfg
            .as_ref()
            .map(|c| &c.mcp_servers)
            .unwrap_or(&run_defaults.mcp_servers);
        servers
            .clone()
            .into_iter()
            .map(
                |(name, entry): (String, fabro_config::mcp::McpServerEntry)| {
                    entry.into_config(name)
                },
            )
            .collect()
    };
    let registry = default_registry(interviewer.clone(), {
        let sandbox_env = sandbox_env.clone();
        let model = model.clone();
        let mcp_servers = mcp_servers.clone();
        move || {
            if dry_run_mode {
                None
            } else {
                let api =
                    AgentApiBackend::new(model.clone(), provider_enum, fallback_chain.clone())
                        .with_env(sandbox_env.clone())
                        .with_mcp_servers(mcp_servers.clone());
                let cli = AgentCliBackend::new(model.clone(), provider_enum)
                    .with_env(sandbox_env.clone());
                Some(Box::new(BackendRouter::new(Box::new(api), cli)))
            }
        }
    });

    // 7. Execute
    // Set up metadata branch for git checkpointing (host or remote — engine fills remote)
    let git = if worktree_path.is_some() {
        Some(GitCheckpointSettings {
            base_sha: worktree_base_sha,
            run_branch: worktree_branch,
            meta_branch: Some(fabro_workflows::git::MetadataStore::branch_name(&run_id)),
        })
    } else {
        None
    };

    let config = RunSettings {
        config: settings_config,
        run_dir: run_dir.clone(),
        cancel_token: None,
        dry_run: dry_run_mode,
        run_id: run_id.clone(),
        labels: label_vec
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        git_author: git_author.clone(),
        workflow_slug: workflow_slug.clone(),
        github_app: github_app.clone(),
        base_branch: existing_record
            .as_ref()
            .and_then(|r| r.base_branch.clone())
            .or(detected_base_branch),
        host_repo_path: existing_record
            .as_ref()
            .and_then(|r| r.host_repo_path.as_deref().map(PathBuf::from))
            .or_else(|| Some(original_cwd.clone())),
        git,
    };

    // Build lifecycle config for sandbox init, setup commands, and devcontainer phases
    let lifecycle = LifecycleConfig {
        setup_commands,
        setup_command_timeout_ms: 300_000,
        devcontainer_phases: if let Some(ref dc) = devcontainer_config {
            vec![
                ("on_create".to_string(), dc.on_create_commands.clone()),
                ("post_create".to_string(), dc.post_create_commands.clone()),
                ("post_start".to_string(), dc.post_start_commands.clone()),
            ]
        } else {
            Vec::new()
        },
    };

    // Defuse the bootstrap guard — engine.run() has taken ownership of lifecycle status.
    status_guard.defuse();

    let run_start = Instant::now();
    let pr_config = if dry_run_mode {
        None
    } else {
        config.pull_request().cloned()
    };
    let started = start(
        validated,
        StartOptions {
            init: fabro_workflows::pipeline::InitOptions {
                run_id: run_id.clone(),
                run_dir: run_dir.clone(),
                dry_run: dry_run_mode,
                emitter: Arc::clone(&emitter),
                sandbox: Arc::clone(&sandbox),
                registry: Arc::new(registry),
                lifecycle,
                run_settings: config,
                hooks: fabro_hooks::HookConfig {
                    hooks: run_cfg
                        .as_ref()
                        .map(|c| c.hooks.clone())
                        .unwrap_or_else(|| run_defaults.hooks.clone()),
                },
                sandbox_env,
                checkpoint: None,
                seed_context: None,
            },
            retro: StartRetroConfig {
                enabled: !no_retro_flag && project_config::is_retro_enabled(),
                dry_run: dry_run_mode,
                llm_client: llm_client.clone(),
                provider: provider_enum,
                model: model.clone(),
            },
            finalize: StartFinalizeConfig {
                preserve_sandbox,
                pr_config,
                github_app: github_app.clone(),
                origin_url: origin_url.clone(),
                model: model.clone(),
            },
        },
    )
    .await;
    let run_duration_ms = run_start.elapsed().as_millis() as u64;
    let mut completion_guard = DetachedRunCompletionGuard::arm(&run_dir);

    // Restore cwd (worktree is kept for `fabro cp` access; pruned separately)
    let _ = std::env::set_current_dir(&original_cwd);
    progress_ui.lock().expect("progress lock poisoned").finish();

    let final_status = match started {
        Ok(started) => {
            if let Some(ref retro) = started.retro {
                print_retro_result(retro, started.retro_duration, &run_dir, styles);
            } else if !no_retro_flag && project_config::is_retro_enabled() {
                eprintln!("\n{}", styles.bold.apply_to("=== Retro ==="));
                eprintln!("{}", styles.dim.apply_to("Retro unavailable"));
            }
            let finalized = started.finalized;
            print_run_conclusion(
                &finalized.conclusion,
                &run_id,
                &run_dir,
                finalized.pushed_branch.as_deref(),
                finalized.pr_url.as_deref(),
                styles,
            );
            print_final_output(&run_dir, styles);
            print_assets(&run_dir, styles);
            finalized.conclusion.status.clone()
        }
        Err(err) => {
            let engine_result = Err(err.clone());
            let (final_status, failure_reason, run_status, status_reason) =
                classify_engine_result(&engine_result);
            let conclusion = build_conclusion(
                &run_dir,
                final_status.clone(),
                failure_reason,
                run_duration_ms,
                None,
            );
            persist_terminal_outcome(&run_dir, &conclusion, run_status, status_reason);
            print_run_conclusion(&conclusion, &run_id, &run_dir, None, None, styles);
            print_final_output(&run_dir, styles);
            print_assets(&run_dir, styles);
            final_status
        }
    };

    completion_guard.defuse();

    fabro_util::run_log::deactivate();
    match final_status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => std::process::exit(1),
    }
}

pub(crate) fn emit_run_notice(
    emitter: &EventEmitter,
    level: RunNoticeLevel,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    emitter.emit(&WorkflowRunEvent::RunNotice {
        level,
        code: code.into(),
        message: message.into(),
    });
}

/// Print a summary of the completed run from `conclusion.json` and `pull_request.json`.
///
/// Used by the unified create+start+attach path in `main.rs` to display
/// the same result block that `run_command` prints in-process.
pub fn print_run_summary(run_dir: &Path, run_id: &str, styles: &Styles) {
    let conclusion_path = run_dir.join("conclusion.json");
    let Ok(conclusion) = fabro_workflows::conclusion::Conclusion::load(&conclusion_path) else {
        return;
    };

    // PR info from pull_request.json (saved by _run_engine)
    let pr_url = std::fs::read_to_string(run_dir.join("pull_request.json"))
        .ok()
        .and_then(|content| {
            serde_json::from_str::<fabro_workflows::pull_request::PullRequestRecord>(&content)
                .ok()
                .map(|record| record.html_url)
        });

    print_run_conclusion(
        &conclusion,
        run_id,
        run_dir,
        None,
        pr_url.as_deref(),
        styles,
    );

    print_final_output(run_dir, styles);
    print_assets(run_dir, styles);
}

pub(crate) fn print_run_conclusion(
    conclusion: &fabro_workflows::conclusion::Conclusion,
    run_id: &str,
    run_dir: &Path,
    pushed_branch: Option<&str>,
    pr_url: Option<&str>,
    styles: &Styles,
) {
    eprintln!("\n{}", styles.bold.apply_to("=== Run Result ==="));
    eprintln!("{}", styles.dim.apply_to(format!("Run:       {run_id}")));

    let status_str = conclusion.status.to_string().to_uppercase();
    let status_color = match conclusion.status {
        StageStatus::Success | StageStatus::PartialSuccess => &styles.bold_green,
        _ => &styles.bold_red,
    };
    eprintln!("Status:    {}", status_color.apply_to(&status_str));
    eprintln!(
        "Duration:  {}",
        HumanDuration(Duration::from_millis(conclusion.duration_ms))
    );

    let total_tokens = conclusion.total_input_tokens + conclusion.total_output_tokens;
    if total_tokens > 0 {
        if conclusion.has_pricing {
            if let Some(cost) = conclusion.total_cost {
                if cost > 0.0 {
                    eprintln!(
                        "{}",
                        styles.dim.apply_to(format!(
                            "Cost:      {} ({} toks)",
                            format_cost(cost),
                            format_tokens_human(total_tokens)
                        ))
                    );
                }
            }
        } else {
            eprintln!(
                "{}",
                styles
                    .dim
                    .apply_to(format!("Toks:      {}", format_tokens_human(total_tokens)))
            );
        }
        if conclusion.total_cache_read_tokens > 0 {
            eprintln!(
                "{}",
                styles.dim.apply_to(format!(
                    "Cache:     {} read, {} write",
                    format_tokens_human(conclusion.total_cache_read_tokens),
                    format_tokens_human(conclusion.total_cache_write_tokens),
                )),
            );
        }
        if conclusion.total_reasoning_tokens > 0 {
            eprintln!(
                "{}",
                styles.dim.apply_to(format!(
                    "Reasoning: {} tokens",
                    format_tokens_human(conclusion.total_reasoning_tokens),
                )),
            );
        }
    }

    eprintln!(
        "{}",
        styles
            .dim
            .apply_to(format!("Run:       {}", tilde_path(run_dir)))
    );

    if let Some(ref failure) = conclusion.failure_reason {
        eprintln!("Failure:   {}", styles.red.apply_to(failure));
    }

    if pushed_branch.is_some() || pr_url.is_some() {
        eprintln!();
        if let Some(branch) = pushed_branch {
            eprintln!("{} {branch}", styles.bold.apply_to("Pushed branch:"));
        }
        if let Some(url) = pr_url {
            eprintln!("{} {url}", styles.bold.apply_to("Pull request:"));
        }
    }
}

pub(crate) fn print_retro_result(
    retro: &fabro_retro::retro::Retro,
    duration: Duration,
    run_dir: &Path,
    styles: &Styles,
) {
    eprintln!("\n{}", styles.bold.apply_to("=== Retro ==="));

    let retro_dur = run_progress::format_duration_short(duration);
    let smoothness_str = retro
        .smoothness
        .as_ref()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let outcome_str = retro.outcome.as_deref().unwrap_or("No outcome recorded");
    let line1_content = format!("Retro: {smoothness_str} \u{2014} {outcome_str}");
    let term_width = console::Term::stderr().size().1 as usize;
    let pad1 = term_width.saturating_sub(line1_content.len() + retro_dur.len());
    eprintln!(
        "{} {}{:pad1$}{}",
        styles.bold.apply_to("Retro:"),
        styles
            .dim
            .apply_to(format!("{smoothness_str} \u{2014} {outcome_str}")),
        "",
        styles.dim.apply_to(&retro_dur),
    );

    let friction_count = retro.friction_points.as_ref().map(|v| v.len()).unwrap_or(0);
    let open_count = retro.open_items.as_ref().map(|v| v.len()).unwrap_or(0);
    if friction_count > 0 || open_count > 0 {
        let mut parts = Vec::new();
        if friction_count > 0 {
            let noun = if friction_count == 1 {
                "friction point"
            } else {
                "friction points"
            };
            parts.push(format!("{friction_count} {noun}"));
        }
        if open_count > 0 {
            let noun = if open_count == 1 {
                "open item"
            } else {
                "open items"
            };
            parts.push(format!("{open_count} {noun}"));
        }
        eprintln!("  {}", styles.dim.apply_to(parts.join(" · ")));
    }

    let retro_path = format!("{}/retro.json", tilde_path(run_dir));
    eprintln!(
        "  {} {}",
        styles.dim.apply_to("Retro saved to"),
        styles.underline.apply_to(&retro_path),
    );
}

/// Print the final stage output from the checkpoint, if available.
pub(crate) fn print_final_output(run_dir: &std::path::Path, styles: &Styles) {
    let Ok(checkpoint) = Checkpoint::load(&run_dir.join("checkpoint.json")) else {
        return;
    };

    // Find the last stage that produced a response (walk completed_nodes in reverse,
    // looking for a "response.{node_id}" entry in context_values).
    for node_id in checkpoint.completed_nodes.iter().rev() {
        let key = format!("response.{node_id}");
        if let Some(serde_json::Value::String(response)) = checkpoint.context_values.get(&key) {
            let text = response.trim();
            if !text.is_empty() {
                eprintln!("\n{}", styles.bold.apply_to("=== Output ==="));
                eprintln!("{}", styles.render_markdown(text));
            }
            return;
        }
    }
}

/// Print collected asset paths, if any.
pub(crate) fn print_assets(run_dir: &std::path::Path, styles: &Styles) {
    let paths = fabro_workflows::asset_snapshot::collect_asset_paths(run_dir);
    if paths.is_empty() {
        return;
    }
    let home = dirs::home_dir();
    eprintln!("\n{}", styles.bold.apply_to("=== Assets ==="));
    for path in &paths {
        let display = match &home {
            Some(home_dir) => {
                let home_str = home_dir.to_string_lossy();
                if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
                    format!("~{rest}")
                } else {
                    path.clone()
                }
            }
            None => path.clone(),
        };
        eprintln!("{display}");
    }
}

/// Validate run configuration without executing the workflow.
///
/// Boots the sandbox (init + cleanup), checks LLM provider availability,
/// resolves the model/provider through the full precedence chain, and prints
/// a styled check report.
#[allow(clippy::too_many_arguments)]
async fn run_preflight(
    graph: &fabro_graphviz::graph::Graph,
    run_cfg: &Option<FabroConfig>,
    args: &RunArgs,
    run_defaults: &FabroConfig,
    git_status: GitSyncStatus,
    sandbox_provider: SandboxProvider,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    origin_url: Option<&str>,
) -> anyhow::Result<()> {
    use fabro_util::check_report::{
        CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus,
    };

    let spinner = indicatif::ProgressBar::new_spinner();
    spinner.set_style(
        indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .expect("valid template")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", ""]),
    );
    spinner.set_message("Running preflight checks…");
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    let mut checks: Vec<CheckResult> = Vec::new();

    // 1. Repository metadata
    let setup_command_count = run_cfg
        .as_ref()
        .and_then(|c| c.setup.as_ref())
        .map_or(0, |s| s.commands.len());

    let repo_summary = origin_url
        .map(|url| {
            let https = fabro_github::ssh_url_to_https(url);
            fabro_github::parse_github_owner_repo(&https)
                .map(|(owner, repo)| format!("{owner}/{repo}"))
                .unwrap_or_else(|_| url.to_string())
        })
        .unwrap_or_else(|| "unknown".into());

    checks.push(CheckResult {
        name: "Repository".into(),
        status: CheckStatus::Pass,
        summary: repo_summary,
        details: vec![
            CheckDetail::new(format!("Setup commands: {setup_command_count}")),
            CheckDetail {
                text: format!("Git: {git_status}"),
                warn: git_status != GitSyncStatus::Synced,
            },
        ],
        remediation: None,
    });

    // 2. Workflow metadata
    let (model, provider) = resolve_model_provider(
        args.model.as_deref(),
        args.provider.as_deref(),
        run_cfg.as_ref(),
        run_defaults,
        graph,
    );

    checks.push(CheckResult {
        name: "Workflow".into(),
        status: CheckStatus::Pass,
        summary: graph.name.clone(),
        details: vec![
            CheckDetail::new(format!("Nodes: {}", graph.nodes.len())),
            CheckDetail::new(format!("Edges: {}", graph.edges.len())),
            CheckDetail::new(format!("Goal: {}", graph.goal())),
        ],
        remediation: None,
    });

    // 2. Sandbox boot check
    let original_cwd = std::env::current_dir()?;
    let daytona_config = resolve_daytona_config(run_cfg.as_ref(), run_defaults);
    #[cfg(feature = "exedev")]
    let exe_config = resolve_exe_config(run_cfg.as_ref(), run_defaults);
    let ssh_config = resolve_ssh_config(run_cfg.as_ref(), run_defaults);

    let sandbox_result: Result<Arc<dyn Sandbox>, String> = match sandbox_provider {
        SandboxProvider::Docker => {
            let config = DockerSandboxConfig {
                host_working_directory: original_cwd.to_string_lossy().to_string(),
                ..DockerSandboxConfig::default()
            };
            DockerSandbox::new(config)
                .map(|env| Arc::new(env) as Arc<dyn Sandbox>)
                .map_err(|e| format!("Docker sandbox creation failed: {e}"))
        }
        SandboxProvider::Daytona => {
            let config = daytona_config.unwrap_or_default();
            match fabro_sandbox::daytona::DaytonaSandbox::new(
                config,
                github_app.clone(),
                None,
                None,
            )
            .await
            {
                Ok(env) => Ok(Arc::new(env) as Arc<dyn Sandbox>),
                Err(e) => Err(format!("Daytona sandbox creation failed: {e}")),
            }
        }
        #[cfg(feature = "exedev")]
        SandboxProvider::Exe => {
            match fabro_sandbox::exe::OpensshRunner::connect_raw("exe.dev").await {
                Ok(mgmt_ssh) => {
                    let config = exe_config.unwrap_or_default();
                    let clone_params = resolve_exe_clone_params(&original_cwd);
                    let env = fabro_sandbox::exe::ExeSandbox::new(
                        Box::new(mgmt_ssh),
                        config,
                        clone_params,
                        None,
                        None,
                    );
                    Ok(Arc::new(env) as Arc<dyn Sandbox>)
                }
                Err(e) => Err(format!("exe.dev SSH connection failed: {e}")),
            }
        }
        SandboxProvider::Ssh => match ssh_config {
            Some(config) => {
                let clone_params = resolve_ssh_clone_params(&original_cwd);
                let env = fabro_sandbox::ssh::SshSandbox::new(config, clone_params, None, None);
                Ok(Arc::new(env) as Arc<dyn Sandbox>)
            }
            None => Err("SSH sandbox requires [sandbox.ssh] config".to_string()),
        },
        SandboxProvider::Local => {
            Ok(Arc::new(LocalSandbox::new(original_cwd.clone())) as Arc<dyn Sandbox>)
        }
    };

    let sandbox_ok = match sandbox_result {
        Ok(sandbox) => match sandbox.initialize().await {
            Ok(()) => {
                let _ = sandbox.cleanup().await;
                true
            }
            Err(e) => {
                let _ = sandbox.cleanup().await;
                checks.push(CheckResult {
                    name: "Sandbox".into(),
                    status: CheckStatus::Error,
                    summary: "failed".into(),
                    details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                    remediation: Some(format!("Sandbox init failed: {e}")),
                });
                false
            }
        },
        Err(e) => {
            checks.push(CheckResult {
                name: "Sandbox".into(),
                status: CheckStatus::Error,
                summary: "failed".into(),
                details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                remediation: Some(e),
            });
            false
        }
    };

    if sandbox_ok {
        checks.push(CheckResult {
            name: "Sandbox".into(),
            status: CheckStatus::Pass,
            summary: sandbox_provider.to_string(),
            details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
            remediation: None,
        });
    }

    // 4. Per-model LLM checks
    let default_provider = provider.as_deref().unwrap_or("anthropic");
    let llm_ok = match fabro_llm::client::Client::from_env().await {
        Ok(c) => {
            let configured: Vec<String> =
                c.provider_names().iter().map(|s| s.to_string()).collect();

            // Collect all distinct (model, provider) pairs from LLM nodes
            let mut model_providers = std::collections::BTreeSet::new();
            for node in graph.nodes.values() {
                if !fabro_graphviz::graph::is_llm_handler_type(node.handler_type()) {
                    continue;
                }
                let node_model = node.model().unwrap_or(&model);
                let node_provider = node.provider().unwrap_or(default_provider);

                // Resolve through catalog to get canonical model ID and provider
                let (resolved_model, resolved_provider) =
                    if let Some(info) = Catalog::builtin().get(node_model) {
                        (info.id.clone(), info.provider.to_string())
                    } else {
                        (node_model.to_string(), node_provider.to_string())
                    };

                // Use node-level provider override if explicitly set, otherwise catalog provider
                let final_provider = if node.provider().is_some() {
                    node_provider.to_string()
                } else {
                    resolved_provider
                };

                model_providers.insert((resolved_model, final_provider));
            }

            // If no LLM nodes found, fall back to the default model/provider
            if model_providers.is_empty() {
                let (resolved_model, resolved_provider) =
                    if let Some(info) = Catalog::builtin().get(&model) {
                        (info.id.clone(), info.provider.to_string())
                    } else {
                        (model.clone(), default_provider.to_string())
                    };
                model_providers.insert((resolved_model, resolved_provider));
            }

            let mut all_ok = true;
            for (model_id, provider_name) in &model_providers {
                match provider_name.parse::<Provider>() {
                    Ok(_) => {
                        let mut status = CheckStatus::Pass;
                        if !configured.iter().any(|n| n == provider_name) {
                            status = CheckStatus::Warning;
                            all_ok = false;
                        }
                        checks.push(CheckResult {
                            name: "LLM".into(),
                            status,
                            summary: model_id.clone(),
                            details: vec![CheckDetail::new(format!("Provider: {provider_name}"))],
                            remediation: if status == CheckStatus::Warning {
                                Some(format!("Provider \"{provider_name}\" is not configured"))
                            } else {
                                None
                            },
                        });
                    }
                    Err(e) => {
                        checks.push(CheckResult {
                            name: "LLM".into(),
                            status: CheckStatus::Error,
                            summary: model_id.clone(),
                            details: vec![CheckDetail::new(format!("Provider: {provider_name}"))],
                            remediation: Some(format!("Invalid provider \"{provider_name}\": {e}")),
                        });
                        all_ok = false;
                    }
                }
            }
            all_ok
        }
        Err(e) => {
            checks.push(CheckResult {
                name: "LLM".into(),
                status: CheckStatus::Error,
                summary: "initialization failed".into(),
                details: vec![],
                remediation: Some(format!("LLM client init failed: {e}")),
            });
            false
        }
    };

    // 5. GitHub token preflight
    let github_permissions = run_cfg
        .as_ref()
        .and_then(|c| c.github.as_ref())
        .or(run_defaults.github.as_ref());
    if let Some(gh_cfg) = github_permissions {
        if !gh_cfg.permissions.is_empty() {
            let perm_details: Vec<CheckDetail> = gh_cfg
                .permissions
                .iter()
                .map(|(k, v)| CheckDetail::new(format!("{k}: {v}")))
                .collect();
            match (&github_app, origin_url) {
                (Some(creds), Some(url)) => {
                    match mint_github_token(creds, url, &gh_cfg.permissions).await {
                        Ok(_) => {
                            checks.push(CheckResult {
                                name: "GitHub Token".into(),
                                status: CheckStatus::Pass,
                                summary: "minted".into(),
                                details: perm_details,
                                remediation: None,
                            });
                        }
                        Err(e) => {
                            checks.push(CheckResult {
                                name: "GitHub Token".into(),
                                status: CheckStatus::Error,
                                summary: "failed".into(),
                                details: perm_details,
                                remediation: Some(format!("Failed to mint GitHub token: {e}")),
                            });
                        }
                    }
                }
                _ => {
                    checks.push(CheckResult {
                        name: "GitHub Token".into(),
                        status: CheckStatus::Warning,
                        summary: "skipped".into(),
                        details: vec![],
                        remediation: Some(
                            "No GitHub App credentials or origin URL available".to_string(),
                        ),
                    });
                }
            }
        }
    }

    // 6. Render report
    spinner.finish_and_clear();

    let report = CheckReport {
        title: "Run Preflight".into(),
        sections: vec![CheckSection {
            title: String::new(),
            checks,
        }],
    };

    let term_width = console::Term::stderr().size().1;
    print!("{}", report.render(styles, true, None, Some(term_width)));

    if sandbox_ok && llm_ok {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

pub(crate) fn build_event_envelope(
    event: &fabro_workflows::event::WorkflowRunEvent,
    run_id: &str,
) -> serde_json::Value {
    detached_support::build_event_envelope(event, run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_run_config_snapshot_copies_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        let toml_content = "version = 1\ngoal = \"test\"\n";
        let toml_path = dir.path().join("original.toml");
        std::fs::write(&toml_path, toml_content).unwrap();

        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        write_run_config_snapshot(&run_dir, Some(toml_path.as_path()))
            .await
            .unwrap();

        let copied = std::fs::read_to_string(run_dir.join(RUN_CONFIG_FILE)).unwrap();
        assert_eq!(copied, toml_content);
    }

    #[tokio::test]
    async fn write_run_config_snapshot_skips_when_none() {
        let dir = tempfile::tempdir().unwrap();
        write_run_config_snapshot(dir.path(), None).await.unwrap();
        assert!(!dir.path().join(RUN_CONFIG_FILE).exists());
    }

    #[test]
    fn resolve_workflow_source_falls_back_to_graph_for_missing_cached_run_config() {
        // Place the test dir inside the runs base so the fallback is allowed.
        let runs_base = fabro_workflows::run_lookup::default_runs_base();
        std::fs::create_dir_all(&runs_base).unwrap();
        let dir = tempfile::tempdir_in(&runs_base).unwrap();
        std::fs::write(dir.path().join(RUN_GRAPH_FILE), "digraph test {}").unwrap();

        let (_resolved_path, dot_path, run_cfg) =
            resolve_workflow_source(&dir.path().join(RUN_CONFIG_FILE)).unwrap();

        assert_eq!(dot_path, dir.path().join(RUN_GRAPH_FILE));
        assert!(run_cfg.is_none());
    }

    #[test]
    fn resolve_workflow_source_errors_for_missing_run_toml_outside_runs_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RUN_GRAPH_FILE), "digraph test {}").unwrap();

        let result = resolve_workflow_source(&dir.path().join(RUN_CONFIG_FILE));
        assert!(result.is_err());
    }

    #[test]
    fn workflow_slug_from_path_uses_file_stem_for_standalone_files() {
        assert_eq!(
            workflow_slug_from_path(Path::new("/tmp/alpha.fabro")).as_deref(),
            Some("alpha")
        );
        assert_eq!(
            workflow_slug_from_path(Path::new("/tmp/beta.toml")).as_deref(),
            Some("beta")
        );
    }

    #[test]
    fn workflow_slug_from_path_uses_parent_for_workflow_files() {
        assert_eq!(
            workflow_slug_from_path(Path::new("/tmp/sluggy/workflow.fabro")).as_deref(),
            Some("sluggy")
        );
        assert_eq!(
            workflow_slug_from_path(Path::new("/tmp/sluggy/workflow.toml")).as_deref(),
            Some("sluggy")
        );
    }

    #[test]
    fn workflow_slug_from_path_uses_final_component_for_extensionless_inputs() {
        assert_eq!(
            workflow_slug_from_path(Path::new("implement-issue")).as_deref(),
            Some("implement-issue")
        );
        assert_eq!(
            workflow_slug_from_path(Path::new("nested/repl")).as_deref(),
            Some("repl")
        );
    }

    #[test]
    fn prepare_workflow_with_project_config_resolves_workflow_toml_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("workflow.fabro"),
            r#"digraph smoke {
    start [shape=Mdiamond, label="Start"]
    exit [shape=Msquare, label="Exit"]
    work [label="Work", prompt="Do the work"]
    start -> work -> exit
}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("workflow.toml"),
            r#"
version = 1
graph = "workflow.fabro"
goal = "toml goal"

[setup]
commands = ["echo from toml"]

[sandbox]
provider = "docker"

[llm]
model = "gpt-5.2"
provider = "openai"

[pull_request]
enabled = true

[assets]
include = ["*.md"]
"#,
        )
        .unwrap();

        let args = RunArgs {
            workflow: Some(dir.path().join("workflow.toml")),
            run_dir: None,
            dry_run: false,
            preflight: false,
            auto_approve: false,
            goal: None,
            goal_file: None,
            model: None,
            provider: None,
            verbose: false,
            sandbox: None,
            label: Vec::new(),
            no_retro: false,
            preserve_sandbox: false,
            detach: false,
            run_id: None,
        };

        let styles = Styles::new(false);
        let prepared = prepare_workflow_with_project_config(
            &args,
            FabroConfig::default(),
            &styles,
            true,
            false,
        )
        .unwrap();

        assert_eq!(prepared.graph().name, "smoke");
        assert_eq!(prepared.graph().goal(), "toml goal");
        assert_eq!(prepared.sandbox_provider, SandboxProvider::Docker);
        assert_eq!(prepared.model, "gpt-5.2");
        assert_eq!(prepared.provider.as_deref(), Some("openai"));

        let run_cfg = prepared
            .run_cfg
            .as_ref()
            .expect("run config should be loaded");
        assert_eq!(
            run_cfg
                .setup
                .as_ref()
                .expect("setup config should be preserved")
                .commands,
            vec!["echo from toml".to_string()]
        );
        assert!(
            run_cfg
                .pull_request
                .as_ref()
                .expect("pull request config should be preserved")
                .enabled
        );
        assert_eq!(
            run_cfg
                .assets
                .as_ref()
                .expect("assets config should be preserved")
                .include,
            vec!["*.md".to_string()]
        );
    }

    #[test]
    fn apply_goal_override_cli_wins_over_toml() {
        use fabro_graphviz::graph::{AttrValue, Graph};
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("original".to_string()),
        );
        apply_goal_override(&mut graph, Some("CLI goal"), Some("TOML goal"));
        assert_eq!(graph.goal(), "CLI goal");
    }

    #[test]
    fn apply_goal_override_toml_wins_over_dot() {
        use fabro_graphviz::graph::{AttrValue, Graph};
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("original".to_string()),
        );
        apply_goal_override(&mut graph, None, Some("TOML goal"));
        assert_eq!(graph.goal(), "TOML goal");
    }

    #[test]
    fn apply_goal_override_noop_when_none() {
        use fabro_graphviz::graph::{AttrValue, Graph};
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("original".to_string()),
        );
        apply_goal_override(&mut graph, None, None);
        assert_eq!(graph.goal(), "original");
    }

    #[test]
    fn resolve_cli_goal_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("goal.md");
        std::fs::write(&path, "goal from file").unwrap();
        let result = resolve_cli_goal(&None, &Some(path)).unwrap();
        assert_eq!(result, Some("goal from file".to_string()));
    }

    #[test]
    fn resolve_cli_goal_from_string() {
        let result = resolve_cli_goal(&Some("inline goal".to_string()), &None).unwrap();
        assert_eq!(result, Some("inline goal".to_string()));
    }

    #[test]
    fn resolve_cli_goal_none() {
        let result = resolve_cli_goal(&None, &None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_model_provider_defaults() {
        let graph = fabro_graphviz::graph::Graph::new("test");
        let defaults = FabroConfig::default();
        let (model, provider) = resolve_model_provider(None, None, None, &defaults, &graph);
        assert_eq!(model, "claude-sonnet-4-6");
        // Catalog resolves anthropic as the provider for claude-sonnet-4-6
        assert_eq!(provider, Some("anthropic".to_string()));
    }

    #[test]
    fn resolve_model_provider_cli_overrides_toml() {
        let graph = fabro_graphviz::graph::Graph::new("test");
        let defaults = FabroConfig::default();
        let cfg = FabroConfig {
            version: Some(1),
            goal: Some("test".to_string()),
            graph: Some("test.fabro".to_string()),
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
                fallbacks: None,
            }),
            ..Default::default()
        };
        let (model, provider) = resolve_model_provider(
            Some("gpt-5.2"),
            Some("openai"),
            Some(&cfg),
            &defaults,
            &graph,
        );
        assert_eq!(model, "gpt-5.2");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_model_provider_toml_overrides_graph() {
        use fabro_graphviz::graph::AttrValue;
        let mut graph = fabro_graphviz::graph::Graph::new("test");
        graph.attrs.insert(
            "default_model".to_string(),
            AttrValue::String("graph-model".to_string()),
        );
        graph.attrs.insert(
            "default_provider".to_string(),
            AttrValue::String("gemini".to_string()),
        );

        let defaults = FabroConfig::default();
        let cfg = FabroConfig {
            version: Some(1),
            goal: Some("test".to_string()),
            graph: Some("test.fabro".to_string()),
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
                fallbacks: None,
            }),
            ..Default::default()
        };
        let (model, provider) = resolve_model_provider(None, None, Some(&cfg), &defaults, &graph);
        assert_eq!(model, "toml-model");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_model_provider_graph_attrs_used_as_fallback() {
        use fabro_graphviz::graph::AttrValue;
        let mut graph = fabro_graphviz::graph::Graph::new("test");
        graph.attrs.insert(
            "default_model".to_string(),
            AttrValue::String("gpt-5.2".to_string()),
        );
        graph.attrs.insert(
            "default_provider".to_string(),
            AttrValue::String("openai".to_string()),
        );

        let defaults = FabroConfig::default();
        let (model, provider) = resolve_model_provider(None, None, None, &defaults, &graph);
        assert_eq!(model, "gpt-5.2");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_model_provider_alias_expansion() {
        let graph = fabro_graphviz::graph::Graph::new("test");
        let defaults = FabroConfig::default();
        let (model, provider) = resolve_model_provider(Some("opus"), None, None, &defaults, &graph);
        assert_eq!(model, "claude-opus-4-6");
        assert_eq!(provider, Some("anthropic".to_string()));
    }

    #[test]
    fn resolve_model_provider_run_defaults_used() {
        let graph = fabro_graphviz::graph::Graph::new("test");
        let defaults = FabroConfig {
            llm: Some(run_config::LlmConfig {
                model: Some("default-model".to_string()),
                provider: Some("openai".to_string()),
                fallbacks: None,
            }),
            ..FabroConfig::default()
        };
        let (model, provider) = resolve_model_provider(None, None, None, &defaults, &graph);
        assert_eq!(model, "default-model");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_model_provider_toml_overrides_run_defaults() {
        let graph = fabro_graphviz::graph::Graph::new("test");
        let defaults = FabroConfig {
            llm: Some(run_config::LlmConfig {
                model: Some("default-model".to_string()),
                provider: Some("anthropic".to_string()),
                fallbacks: None,
            }),
            ..FabroConfig::default()
        };
        let cfg = FabroConfig {
            version: Some(1),
            goal: Some("test".to_string()),
            graph: Some("test.fabro".to_string()),
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
                fallbacks: None,
            }),
            ..Default::default()
        };
        let (model, provider) = resolve_model_provider(None, None, Some(&cfg), &defaults, &graph);
        assert_eq!(model, "toml-model");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_preserve_sandbox_cli_wins() {
        let cfg = FabroConfig {
            sandbox: Some(sandbox_config::SandboxConfig {
                provider: None,
                preserve: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let defaults = FabroConfig::default();
        assert!(resolve_preserve_sandbox(true, Some(&cfg), &defaults));
    }

    #[test]
    fn resolve_preserve_sandbox_toml_wins_over_defaults() {
        let cfg = FabroConfig {
            sandbox: Some(sandbox_config::SandboxConfig {
                provider: None,
                preserve: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let defaults = FabroConfig {
            sandbox: Some(sandbox_config::SandboxConfig {
                provider: None,
                preserve: Some(false),
                ..Default::default()
            }),
            ..FabroConfig::default()
        };
        assert!(resolve_preserve_sandbox(false, Some(&cfg), &defaults));
    }

    #[test]
    fn resolve_preserve_sandbox_defaults_used() {
        let defaults = FabroConfig {
            sandbox: Some(sandbox_config::SandboxConfig {
                provider: None,
                preserve: Some(true),
                ..Default::default()
            }),
            ..FabroConfig::default()
        };
        assert!(resolve_preserve_sandbox(false, None, &defaults));
    }

    #[test]
    fn resolve_preserve_sandbox_defaults_to_false() {
        let defaults = FabroConfig::default();
        assert!(!resolve_preserve_sandbox(false, None, &defaults));
    }

    #[test]
    fn resolve_worktree_mode_defaults_to_clean() {
        let defaults = FabroConfig::default();
        assert_eq!(
            resolve_worktree_mode(None, &defaults),
            sandbox_config::WorktreeMode::Clean
        );
    }

    #[test]
    fn resolve_worktree_mode_from_toml() {
        let cfg = FabroConfig {
            sandbox: Some(sandbox_config::SandboxConfig {
                local: Some(sandbox_config::LocalSandboxConfig {
                    worktree_mode: sandbox_config::WorktreeMode::Always,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let defaults = FabroConfig::default();
        assert_eq!(
            resolve_worktree_mode(Some(&cfg), &defaults),
            sandbox_config::WorktreeMode::Always
        );
    }

    #[test]
    fn resolve_worktree_mode_from_defaults() {
        let defaults = FabroConfig {
            sandbox: Some(sandbox_config::SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: Some(sandbox_config::LocalSandboxConfig {
                    worktree_mode: sandbox_config::WorktreeMode::Dirty,
                }),
                ..Default::default()
            }),
            ..FabroConfig::default()
        };
        assert_eq!(
            resolve_worktree_mode(None, &defaults),
            sandbox_config::WorktreeMode::Dirty
        );
    }

    #[test]
    fn resolve_worktree_mode_toml_overrides_defaults() {
        let cfg = FabroConfig {
            sandbox: Some(sandbox_config::SandboxConfig {
                local: Some(sandbox_config::LocalSandboxConfig {
                    worktree_mode: sandbox_config::WorktreeMode::Never,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let defaults = FabroConfig {
            sandbox: Some(sandbox_config::SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: Some(sandbox_config::LocalSandboxConfig {
                    worktree_mode: sandbox_config::WorktreeMode::Dirty,
                }),
                ..Default::default()
            }),
            ..FabroConfig::default()
        };
        assert_eq!(
            resolve_worktree_mode(Some(&cfg), &defaults),
            sandbox_config::WorktreeMode::Never
        );
    }

    #[test]
    fn redact_removes_aws_key_from_compact_json() {
        let envelope = serde_json::json!({
            "timestamp": "2025-01-01T00:00:00.000Z",
            "run_id": "abc-123",
            "event": {
                "type": "agent",
                "content": "My key is AKIAYRWQG5EJLPZLBYNP and secret is wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
            }
        });
        let compact = serde_json::to_string(&envelope).unwrap();
        let redacted = fabro_util::redact::redact_jsonl_line(&compact);

        assert!(!redacted.contains("AKIAYRWQG5EJLPZLBYNP"));
        assert!(redacted.contains("REDACTED"));

        let parsed: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(parsed["run_id"], "abc-123");
        assert_eq!(parsed["timestamp"], "2025-01-01T00:00:00.000Z");
    }

    #[test]
    fn redact_removes_aws_key_from_pretty_json() {
        let envelope = serde_json::json!({
            "timestamp": "2025-01-01T00:00:00.000Z",
            "run_id": "def-456",
            "event": {
                "type": "agent",
                "content": "Credentials: AKIAYRWQG5EJLPZLBYNP"
            }
        });
        let pretty = serde_json::to_string_pretty(&envelope).unwrap();
        let redacted = fabro_util::redact::redact_jsonl_line(&pretty);

        assert!(!redacted.contains("AKIAYRWQG5EJLPZLBYNP"));
        assert!(redacted.contains("REDACTED"));

        let parsed: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(parsed["run_id"], "def-456");
    }

    #[test]
    fn envelope_field_order_starts_with_ts_run_id_event() {
        let event = fabro_workflows::event::WorkflowRunEvent::StageStarted {
            node_id: "plan".to_string(),
            name: "Plan".to_string(),
            index: 0,
            handler_type: Some("agent".to_string()),
            script: None,
            attempt: 1,
            max_attempts: 3,
        };
        let envelope = build_event_envelope(&event, "run-123");
        let json = serde_json::to_string(&envelope).unwrap();
        // Parse the raw JSON to get field order
        let fields: Vec<String> = json
            .trim_start_matches('{')
            .trim_end_matches('}')
            .split(',')
            .filter_map(|pair| {
                let key = pair.split(':').next()?;
                Some(key.trim().trim_matches('"').to_string())
            })
            .collect();
        assert_eq!(&fields[0], "ts", "first field must be ts, got: {fields:?}");
        assert_eq!(
            &fields[1], "run_id",
            "second field must be run_id, got: {fields:?}"
        );
        assert_eq!(
            &fields[2], "event",
            "third field must be event, got: {fields:?}"
        );
    }
}
