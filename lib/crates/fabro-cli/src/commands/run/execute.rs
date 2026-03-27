use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{bail, Context};
use chrono::Local;
use fabro_agent::{DockerSandbox, DockerSandboxConfig, LocalSandbox, Sandbox};
use fabro_config::config::FabroConfig;
use fabro_config::{project as project_config, run as run_config, sandbox as sandbox_config};
use fabro_interview::{AutoApproveInterviewer, ConsoleInterviewer, FileInterviewer, Interviewer};
use fabro_model::{Catalog, FallbackTarget, Provider};
use fabro_sandbox::SandboxProvider;
use fabro_util::terminal::Styles;
use fabro_workflows::event::EventEmitter;
use fabro_workflows::git::GitSyncStatus;
use fabro_workflows::operations::{
    resume as operations_resume, start, DevcontainerSpec, LlmSpec, SandboxEnvSpec, SandboxSpec,
    StartFinalizeOptions, StartOptions, StartPullRequestConfig, StartRetroOptions,
};
use fabro_workflows::outcome::StageStatus;
use fabro_workflows::outcome::{compute_stage_cost, format_cost};
use fabro_workflows::pipeline::{
    build_conclusion, classify_engine_result, persist_terminal_outcome, Persisted, Validated,
};
use fabro_workflows::records::Checkpoint;
use fabro_workflows::run_options::LifecycleOptions;
use indicatif::HumanDuration;
use std::time::Duration;
use tracing::debug;

use super::detached_support::{DetachedRunBootstrapGuard, DetachedRunCompletionGuard};
use super::run_progress;
use crate::args::{CliSandboxProvider, GlobalArgs, RunArgs};
use crate::cli_config;
use crate::shared::{
    format_tokens_human, print_diagnostics, read_workflow_file, relative_path, tilde_path,
};

/// Resolve goal from `--goal` string or `--goal-file` path.
pub(crate) fn resolve_cli_goal(
    goal: Option<&str>,
    goal_file: Option<&Path>,
) -> anyhow::Result<Option<String>> {
    match (goal, goal_file) {
        (Some(g), _) => Ok(Some(g.to_string())),
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

pub(crate) use fabro_workflows::operations::default_run_dir;

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

pub(crate) struct ExecutionOverrides<'a> {
    pub dry_run: bool,
    pub auto_approve: bool,
    pub no_retro: bool,
    pub verbose: bool,
    pub preserve_sandbox: bool,
    pub model: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub sandbox_provider: SandboxProvider,
}

pub(crate) fn apply_execution_overrides(config: &mut FabroConfig, overrides: &ExecutionOverrides) {
    config.dry_run = Some(overrides.dry_run);
    config.auto_approve = Some(overrides.auto_approve);
    config.no_retro = Some(overrides.no_retro);
    config.verbose = Some(overrides.verbose);

    if let Some(model) = overrides.model {
        config.llm.get_or_insert_default().model = Some(model.to_string());
    }
    if let Some(provider) = overrides.provider {
        config.llm.get_or_insert_default().provider = Some(provider.to_string());
    }

    config.sandbox.get_or_insert_default().provider = Some(overrides.sandbox_provider.to_string());
    if overrides.preserve_sandbox {
        config.sandbox.get_or_insert_default().preserve = Some(true);
    }
}

pub(crate) fn parse_labels(labels: &[String]) -> HashMap<String, String> {
    labels
        .iter()
        .filter_map(|label| label.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn print_workflow_header(
    graph: &fabro_graphviz::graph::Graph,
    diagnostics: &[fabro_validate::Diagnostic],
    dot_path: &Path,
    styles: &Styles,
) {
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
        styles.dim.apply_to(relative_path(dot_path)),
    );

    let goal = graph.goal();
    if !goal.is_empty() {
        let stripped = fabro_util::text::strip_goal_decoration(goal);
        eprintln!("{} {stripped}\n", styles.bold.apply_to("Goal:"));
    }

    print_diagnostics(diagnostics, styles);
}

pub(crate) fn print_workflow_report(validated: &Validated, dot_path: &Path, styles: &Styles) {
    print_workflow_header(validated.graph(), validated.diagnostics(), dot_path, styles);
}

pub(crate) fn print_workflow_report_from_persisted(
    persisted: &Persisted,
    dot_path: &Path,
    styles: &Styles,
) {
    print_workflow_header(persisted.graph(), persisted.diagnostics(), dot_path, styles);
}

pub(crate) fn print_diagnostics_from_error(
    diagnostics: &[fabro_validate::Diagnostic],
    styles: &Styles,
) {
    print_diagnostics(diagnostics, styles);
}

pub(crate) struct WorkflowSourceInput {
    pub raw_source: String,
    pub config: FabroConfig,
    pub workflow_slug: Option<String>,
    pub run_defaults: FabroConfig,
    pub workflow_toml_path: Option<PathBuf>,
    pub dot_path: PathBuf,
    pub goal_override: Option<String>,
}

#[allow(dead_code)]
enum WorkflowState {
    Source(Box<WorkflowSourceInput>),
    Persisted(Box<Persisted>),
}

pub(crate) fn load_workflow_source_input(
    workflow: &Path,
    goal: Option<&str>,
    goal_file: Option<&Path>,
    mut run_defaults: FabroConfig,
    apply_project_config: bool,
) -> anyhow::Result<WorkflowSourceInput> {
    if apply_project_config {
        // Apply project-level config overrides (fabro.toml) on top of CLI defaults.
        if let Ok(Some((_config_path, project_config))) =
            project_config::discover_project_config(&std::env::current_dir().unwrap_or_default())
        {
            tracing::debug!("Applying run defaults from fabro.toml");
            run_defaults.merge_overlay(project_config);
        }
    }

    // Resolve workflow arg, load run config if TOML, merge with defaults.
    let (resolved_workflow_path, dot_path, config) = {
        let (resolved, dot, cfg) = resolve_workflow_source(workflow)?;
        match cfg {
            Some(cfg) => {
                let mut merged = run_defaults.clone();
                merged.merge_overlay(cfg);
                (resolved, dot, merged)
            }
            None => (resolved, dot, run_defaults.clone()),
        }
    };
    let workflow_slug = workflow_slug_from_path(&resolved_workflow_path);

    if let Some(dir) = config.work_dir.as_deref() {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("Failed to set working directory to {dir}: {e}"))?;
    }

    let raw_source = read_workflow_file(&dot_path)?;
    let cli_goal = resolve_cli_goal(goal, goal_file)?;
    let goal_override = cli_goal.or_else(|| config.goal.clone());

    let workflow_toml_path = if resolved_workflow_path
        .extension()
        .is_some_and(|ext| ext == "toml")
    {
        Some(resolved_workflow_path)
    } else {
        None
    };

    Ok(WorkflowSourceInput {
        raw_source,
        config,
        workflow_slug,
        run_defaults,
        workflow_toml_path,
        dot_path,
        goal_override,
    })
}

/// Pre-prepared run state, used to skip workflow preparation in `run_command_impl`.
struct RecordBasedRun {
    workflow: WorkflowState,
    run_defaults: FabroConfig,
}

/// Execute a workflow run from a saved RunRecord, bypassing workflow preparation.
///
/// Used by `run_engine_entrypoint` for detached runs that already have a RunRecord on disk.
pub async fn run_from_record(
    persisted: Persisted,
    run_dir: PathBuf,
    run_defaults: FabroConfig,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<()> {
    let record = persisted.run_record().clone();
    let record_run = RecordBasedRun {
        workflow: WorkflowState::Persisted(Box::new(persisted)),
        run_defaults,
    };

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

    let args = RunArgs {
        workflow: None,
        run_dir: Some(run_dir),
        dry_run: record.config.dry_run_enabled(),

        auto_approve: record.config.auto_approve_enabled(),
        goal: record.config.goal.clone(),
        goal_file: None,
        model: Some(model),
        provider,
        verbose: record.config.verbose_enabled(),
        sandbox: Some(CliSandboxProvider::from(sandbox_provider)),
        label: record
            .labels
            .iter()
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
        run_id: Some(record.run_id.clone()),
    };

    run_command_impl(
        args,
        styles,
        github_app,
        git_author,
        Some(record_run),
        false,
    )
    .await
}

/// Resume an existing workflow run from its persisted checkpoint.
pub async fn resume_from_record(
    persisted: Persisted,
    run_dir: PathBuf,
    run_defaults: FabroConfig,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<()> {
    let record = persisted.run_record().clone();
    let record_run = RecordBasedRun {
        workflow: WorkflowState::Persisted(Box::new(persisted)),
        run_defaults,
    };

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

    let args = RunArgs {
        workflow: None,
        run_dir: Some(run_dir),
        dry_run: record.config.dry_run_enabled(),

        auto_approve: record.config.auto_approve_enabled(),
        goal: record.config.goal.clone(),
        goal_file: None,
        model: Some(model),
        provider,
        verbose: record.config.verbose_enabled(),
        sandbox: Some(CliSandboxProvider::from(sandbox_provider)),
        label: record
            .labels
            .iter()
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
        run_id: Some(record.run_id.clone()),
    };

    run_command_impl(args, styles, github_app, git_author, Some(record_run), true).await
}

fn ensure_resume_target_is_not_already_successful(run_dir: &Path) -> anyhow::Result<()> {
    const MESSAGE: &str = "run already finished successfully — nothing to resume";

    if let Ok(record) =
        fabro_workflows::run_status::RunStatusRecord::load(&run_dir.join("status.json"))
    {
        if record.status == fabro_workflows::run_status::RunStatus::Succeeded {
            bail!(MESSAGE);
        }
    }

    if let Ok(conclusion) =
        fabro_workflows::records::Conclusion::load(&run_dir.join("conclusion.json"))
    {
        if matches!(
            conclusion.status,
            StageStatus::Success | StageStatus::PartialSuccess | StageStatus::Skipped
        ) {
            bail!(MESSAGE);
        }
    }

    Ok(())
}

/// Execute a full workflow run.
///
/// # Errors
///
/// Returns an error if the workflow cannot be read, parsed, validated, or executed.
pub async fn execute(mut args: RunArgs, _globals: &GlobalArgs) -> anyhow::Result<()> {
    let styles: &'static fabro_util::terminal::Styles =
        Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
    let cli_config = cli_config::load_cli_config(None)?;
    args.verbose = args.verbose || cli_config.verbose_enabled();

    let quiet = args.detach;
    let _prevent_idle_sleep = cli_config.prevent_idle_sleep_enabled();
    let (run_id, run_dir) = super::create::create_run(&args, cli_config, styles, quiet).await?;

    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = fabro_beastie::guard(_prevent_idle_sleep);

    let child = super::start::start_run(&run_dir, false)?;

    if args.detach {
        println!("{run_id}");
    } else {
        let exit_code = super::attach::attach_run(&run_dir, true, styles, Some(child)).await?;
        print_run_summary(&run_dir, &run_id, styles);
        if exit_code != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn run_command_impl(
    args: RunArgs,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
    record_run: Option<RecordBasedRun>,
    resume: bool,
) -> anyhow::Result<()> {
    let (workflow, run_defaults) = match record_run {
        Some(rr) => (rr.workflow, rr.run_defaults),
        None => unreachable!("run_command_impl always receives a RecordBasedRun"),
    };

    // For record-based runs from run_from_record, the workflow has already been persisted.
    let from_record = matches!(&workflow, WorkflowState::Persisted(_)) && args.workflow.is_none();

    // Pre-flight: check git cleanliness before creating any files
    let original_cwd = std::env::current_dir()?;
    let (origin_url, detected_base_branch) =
        fabro_sandbox::daytona::detect_repo_info(&original_cwd)
            .map(|(url, branch)| (Some(url), branch))
            .unwrap_or((None, None));

    // 3. Create logs directory
    // Extract values from args before partial move
    let dry_run_flag = args.dry_run;
    let auto_approve_flag = args.auto_approve;
    let no_retro_flag = args.no_retro;
    let verbose_flag = args.verbose;
    let preserve_sandbox_flag = args.preserve_sandbox;
    let label_vec = args.label.clone();
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    let run_dir = args
        .run_dir
        .clone()
        .unwrap_or_else(|| default_run_dir(&run_id, dry_run_flag));
    if resume {
        ensure_resume_target_is_not_already_successful(&run_dir)?;
    }
    let cached_run_restart = match &workflow {
        WorkflowState::Source(_) if !from_record => {
            let workflow_path = args.workflow.as_ref().unwrap();
            is_cached_run_restart(workflow_path, &run_dir)
        }
        _ => false,
    };

    let (persisted, raw_source, workflow_toml_path) = match workflow {
        WorkflowState::Persisted(persisted) => (*persisted, String::new(), None),
        WorkflowState::Source(source_input) if cached_run_restart => (
            Persisted::load(&run_dir)?,
            source_input.raw_source,
            source_input.workflow_toml_path,
        ),
        WorkflowState::Source(source_input) => {
            let mut config = source_input.config.clone();
            let sandbox_provider = if dry_run_flag {
                SandboxProvider::Local
            } else {
                resolve_sandbox_provider(
                    args.sandbox.map(Into::into),
                    Some(&config),
                    &source_input.run_defaults,
                )?
            };
            apply_execution_overrides(
                &mut config,
                &ExecutionOverrides {
                    dry_run: dry_run_flag,
                    auto_approve: auto_approve_flag,
                    no_retro: no_retro_flag,
                    verbose: verbose_flag,
                    preserve_sandbox: preserve_sandbox_flag,
                    model: args.model.as_deref(),
                    provider: args.provider.as_deref(),
                    sandbox_provider,
                },
            );

            match fabro_workflows::operations::create(
                &source_input.raw_source,
                fabro_workflows::operations::RunCreateOptions {
                    config,
                    run_dir: Some(run_dir.clone()),
                    run_id: Some(run_id.clone()),
                    workflow_slug: source_input.workflow_slug.clone(),
                    labels: parse_labels(&label_vec),
                    base_branch: detected_base_branch.clone(),
                    working_directory: Some(original_cwd.clone()),
                    host_repo_path: Some(original_cwd.to_string_lossy().to_string()),
                    goal_override: source_input.goal_override.clone(),
                    base_dir: Some(
                        source_input
                            .dot_path
                            .parent()
                            .unwrap_or(Path::new("."))
                            .to_path_buf(),
                    ),
                },
            ) {
                Ok(persisted) => {
                    print_workflow_report_from_persisted(
                        &persisted,
                        &source_input.dot_path,
                        styles,
                    );
                    (
                        persisted,
                        source_input.raw_source,
                        source_input.workflow_toml_path,
                    )
                }
                Err(fabro_workflows::error::FabroError::ValidationFailed { diagnostics }) => {
                    print_diagnostics_from_error(&diagnostics, styles);
                    bail!("Validation failed");
                }
                Err(err) => return Err(err.into()),
            }
        }
    };
    let mut run_cfg = Some(persisted.run_record().config.clone());
    let sandbox_provider = run_cfg
        .as_ref()
        .and_then(|cfg| cfg.sandbox.as_ref())
        .and_then(|sandbox| sandbox.provider.as_deref())
        .unwrap_or("local")
        .parse()
        .unwrap_or(SandboxProvider::Local);
    let model = run_cfg
        .as_ref()
        .and_then(|cfg| cfg.llm.as_ref())
        .and_then(|llm| llm.model.clone())
        .unwrap_or_default();
    let provider = run_cfg
        .as_ref()
        .and_then(|cfg| cfg.llm.as_ref())
        .and_then(|llm| llm.provider.clone())
        .filter(|value| !value.is_empty());
    let preserve_sandbox =
        resolve_preserve_sandbox(args.preserve_sandbox, run_cfg.as_ref(), &run_defaults);
    let setup_commands: Vec<String> = run_cfg
        .as_ref()
        .and_then(|c| c.setup.as_ref())
        .or(run_defaults.setup.as_ref())
        .map(|s| s.commands.clone())
        .unwrap_or_default();

    tokio::fs::create_dir_all(&run_dir).await?;
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

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let daytona_config = resolve_daytona_config(run_cfg.as_ref(), &run_defaults);
    #[cfg(feature = "exedev")]
    let exe_config = resolve_exe_config(run_cfg.as_ref(), &run_defaults);
    let ssh_config = resolve_ssh_config(run_cfg.as_ref(), &run_defaults);
    let emitter = Arc::new(emitter);

    // Parse provider string to enum (defaults to best available from env)
    let provider_enum: Provider = provider
        .as_deref()
        .map(|s| s.parse::<Provider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or_else(Provider::default_from_env);

    let fallback_chain = resolve_fallback_chain(provider_enum, &model, run_cfg.as_ref());
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
    let sandbox_spec = match sandbox_provider {
        SandboxProvider::Local => SandboxSpec::Local {
            working_directory: cwd.clone(),
        },
        SandboxProvider::Docker => SandboxSpec::Docker {
            config: DockerSandboxConfig {
                host_working_directory: cwd.to_string_lossy().to_string(),
                ..DockerSandboxConfig::default()
            },
        },
        SandboxProvider::Daytona => SandboxSpec::Daytona {
            config: daytona_config.unwrap_or_default(),
            github_app: github_app.clone(),
            run_id: Some(run_id.clone()),
            clone_branch: detected_base_branch.clone(),
        },
        #[cfg(feature = "exedev")]
        SandboxProvider::Exe => SandboxSpec::Exe {
            config: exe_config.unwrap_or_default(),
            clone_params: resolve_exe_clone_params(&original_cwd),
            run_id: Some(run_id.clone()),
            github_app: github_app.clone(),
            mgmt_destination: "exe.dev".to_string(),
        },
        #[cfg(not(feature = "exedev"))]
        SandboxProvider::Exe => {
            bail!("exe sandbox requires the exedev feature");
        }
        SandboxProvider::Ssh => SandboxSpec::Ssh {
            config: ssh_config
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--sandbox ssh requires [sandbox.ssh] config"))?,
            clone_params: resolve_ssh_clone_params(&original_cwd),
            run_id: Some(run_id.clone()),
            github_app: github_app.clone(),
        },
    };

    let toml_env = if let Some(mut env) = run_cfg
        .as_ref()
        .and_then(|c| c.sandbox.as_ref())
        .or(run_defaults.sandbox.as_ref())
        .and_then(|s| s.env.clone())
    {
        if run_cfg.is_none() {
            run_config::resolve_env_refs(&mut env)?;
        }
        env
    } else {
        HashMap::new()
    };

    let sandbox_env = SandboxEnvSpec {
        devcontainer_env: HashMap::new(),
        toml_env,
        github_permissions: run_cfg
            .as_ref()
            .and_then(|c| c.github.as_ref())
            .or(run_defaults.github.as_ref())
            .and_then(|cfg| (!cfg.permissions.is_empty()).then(|| cfg.permissions.clone())),
        origin_url: origin_url.clone(),
    };

    let devcontainer_enabled = run_cfg
        .as_ref()
        .and_then(|c| c.sandbox.as_ref())
        .or(run_defaults.sandbox.as_ref())
        .and_then(|s| s.devcontainer)
        .unwrap_or(false);

    let llm = LlmSpec {
        model: model.clone(),
        provider: provider_enum,
        fallback_chain,
        mcp_servers,
        dry_run: dry_run_flag,
    };

    let worktree_mode = resolve_worktree_mode(run_cfg.as_ref(), &run_defaults);
    let lifecycle = LifecycleOptions {
        setup_commands,
        setup_command_timeout_ms: 300_000,
        devcontainer_phases: Vec::new(),
    };

    // Defuse the bootstrap guard — engine.run() has taken ownership of lifecycle status.
    status_guard.defuse();

    let run_start = Instant::now();
    let pr_config = persisted.run_record().config.pull_request.clone();
    let start_options = StartOptions {
        cancel_token: None,
        emitter: Arc::clone(&emitter),
        sandbox: sandbox_spec,
        llm,
        interviewer: interviewer.clone(),
        lifecycle,
        hooks: fabro_hooks::HookConfig {
            hooks: run_cfg
                .as_ref()
                .map(|c| c.hooks.clone())
                .unwrap_or_else(|| run_defaults.hooks.clone()),
        },
        sandbox_env,
        devcontainer: devcontainer_enabled.then(|| DevcontainerSpec {
            enabled: true,
            resolve_dir: cwd.clone(),
        }),
        seed_context: None,
        git_author,
        git: None,
        github_app: github_app.clone(),
        worktree_mode: Some(worktree_mode),
        registry_override: None,
        dry_run: dry_run_flag,
        retro: StartRetroOptions {
            enabled: !no_retro_flag && project_config::is_retro_enabled(),
        },
        finalize: StartFinalizeOptions { preserve_sandbox },
        pull_request: StartPullRequestConfig {
            pr_config,
            github_app: github_app.clone(),
            origin_url: origin_url.clone(),
            model: model.clone(),
        },
    };
    let started = if resume {
        operations_resume(&run_dir, start_options).await
    } else {
        start(&run_dir, start_options).await
    };
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

/// Print a summary of the completed run from `conclusion.json` and `pull_request.json`.
///
/// Used by the unified create+start+attach path in `main.rs` to display
/// the same result block that `run_command` prints in-process.
pub fn print_run_summary(run_dir: &Path, run_id: &str, styles: &Styles) {
    let conclusion_path = run_dir.join("conclusion.json");
    let Ok(conclusion) = fabro_workflows::records::Conclusion::load(&conclusion_path) else {
        return;
    };

    // PR info from pull_request.json (saved by __detached)
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
    conclusion: &fabro_workflows::records::Conclusion,
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
pub(crate) async fn run_preflight(
    graph: &fabro_graphviz::graph::Graph,
    run_cfg: &Option<FabroConfig>,
    cli_model: Option<&str>,
    cli_provider: Option<&str>,
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
        cli_model,
        cli_provider,
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
        #[cfg(not(feature = "exedev"))]
        SandboxProvider::Exe => Err("exe sandbox requires the exedev feature".to_string()),
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

#[cfg(test)]
pub(crate) fn build_event_envelope(
    event: &fabro_workflows::event::WorkflowRunEvent,
    run_id: &str,
) -> serde_json::Value {
    fabro_workflows::event::build_event_envelope(event, run_id)
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
    fn load_workflow_source_input_resolves_workflow_toml_settings() {
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

        let workflow_path = dir.path().join("workflow.toml");

        let source_input =
            load_workflow_source_input(&workflow_path, None, None, FabroConfig::default(), false)
                .unwrap();
        let validated = fabro_workflows::operations::validate(
            &source_input.raw_source,
            fabro_workflows::operations::ValidateOptions {
                base_dir: Some(dir.path().to_path_buf()),
                config: Some(source_input.config.clone()),
                goal_override: source_input.goal_override.clone(),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(validated.graph().name, "smoke");
        assert_eq!(validated.graph().goal(), "toml goal");
        let (model, provider) = resolve_model_provider(
            None,
            None,
            Some(&source_input.config),
            &source_input.run_defaults,
            validated.graph(),
        );
        assert_eq!(model, "gpt-5.2");
        assert_eq!(provider.as_deref(), Some("openai"));
        let sandbox_provider =
            resolve_sandbox_provider(None, Some(&source_input.config), &source_input.run_defaults)
                .unwrap();
        assert_eq!(sandbox_provider, SandboxProvider::Docker);

        let run_cfg = &source_input.config;
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
    fn resolve_cli_goal_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("goal.md");
        std::fs::write(&path, "goal from file").unwrap();
        let result = resolve_cli_goal(None, Some(path.as_path())).unwrap();
        assert_eq!(result, Some("goal from file".to_string()));
    }

    #[test]
    fn resolve_cli_goal_from_string() {
        let result = resolve_cli_goal(Some("inline goal"), None).unwrap();
        assert_eq!(result, Some("inline goal".to_string()));
    }

    #[test]
    fn resolve_cli_goal_none() {
        let result = resolve_cli_goal(None, None).unwrap();
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
