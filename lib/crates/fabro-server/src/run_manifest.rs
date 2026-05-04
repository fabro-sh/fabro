use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use fabro_api::types;
use fabro_auth::auth_issue_message;
use fabro_config::{
    CliLayer, CliOutputLayer, DaytonaDockerfileLayer, DockerSandboxLayer, LocalSandboxLayer,
    ReplaceMap, RunExecutionLayer, RunLayer, RunModelLayer, RunSandboxLayer,
    WorkflowSettingsBuilder,
};
use fabro_graphviz::graph::{Graph, is_llm_handler_type};
use fabro_graphviz::render::apply_direction;
use fabro_llm::Provider;
use fabro_llm::model_test::{ModelTestMode, ModelTestStatus, run_model_test};
use fabro_model::{Catalog, Model, ModelCosts, ModelFeatures, ModelLimits};
use fabro_sandbox::config::{
    DaytonaNetwork, DaytonaSnapshotSettings, DockerfileSource as SandboxDockerfileSource,
};
use fabro_sandbox::daytona::DaytonaConfig;
use fabro_sandbox::redact::redact_auth_url;
use fabro_sandbox::{DockerSandboxOptions, Sandbox, SandboxProvider, SandboxSpec};
use fabro_static::EnvVars;
use fabro_types::settings::ServerNamespace;
use fabro_types::settings::cli::OutputVerbosity;
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::run::{
    ApprovalMode, DaytonaNetworkLayer, DaytonaSettings, DockerSettings, DockerfileSource, RunGoal,
    RunMode, RunNamespace, WorktreeMode,
};
use fabro_types::{RunId, WorkflowSettings};
use fabro_util::check_report::{CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus};
use fabro_validate::Severity;
use fabro_workflow::operations::{CreateRunInput, ValidateInput, WorkflowInput, validate};
use fabro_workflow::pipeline::Validated;
use fabro_workflow::run_materialization::materialize_run;
use fabro_workflow::workflow_bundle::{BundledWorkflow, ParsedWorkflowConfig, WorkflowBundle};
use fabro_workflow::{Error as WorkflowError, ManifestPath};
use tokio::process::Command;
use tokio::time;

use crate::server::AppState;

#[derive(Clone)]
pub(crate) struct PreparedManifest {
    pub cwd:              PathBuf,
    pub git:              Option<types::GitContext>,
    pub root_source:      String,
    pub run_id:           Option<RunId>,
    pub settings:         WorkflowSettings,
    pub target_path:      ManifestPath,
    pub workflow_bundle:  WorkflowBundle,
    pub workflow_input:   BundledWorkflow,
    pub source_directory: PathBuf,
    pub in_place:         bool,
}

#[derive(Clone, Debug, Default)]
struct ManifestSettingsOverrides {
    run: Option<RunLayer>,
    cli: Option<CliLayer>,
}

#[cfg(test)]
pub(crate) fn manifest_run_defaults(run: Option<&RunLayer>) -> RunLayer {
    run.cloned().unwrap_or_default()
}

pub(crate) fn prepare_manifest(
    manifest_run_defaults: &RunLayer,
    manifest: &types::RunManifest,
) -> Result<PreparedManifest> {
    if manifest.version != 1 {
        bail!("unsupported manifest version {}", manifest.version);
    }

    let cwd = PathBuf::from(&manifest.cwd);
    let target_path = ManifestPath::from_wire(&manifest.target.path)
        .ok_or_else(|| anyhow!("invalid manifest target path: {}", manifest.target.path))?;
    let workflow_bundle = workflow_bundle_from_manifest(&manifest.workflows)?;
    let workflow_input = workflow_bundle
        .workflow(&target_path)
        .cloned()
        .ok_or_else(|| anyhow!("manifest target path is missing from workflows map"))?;
    let root_source = workflow_input.source.clone();

    let args_overrides = manifest_args_overrides(manifest.args.as_ref());
    let workflow_run_layer = root_workflow_run_layer(&workflow_input)?;
    let mut workflow_settings_builder =
        WorkflowSettingsBuilder::new().server_run_defaults(manifest_run_defaults.clone());
    if let Some(run) = args_overrides.run {
        workflow_settings_builder = workflow_settings_builder.run_overrides(run);
    }
    if let Some(cli) = args_overrides.cli {
        workflow_settings_builder = workflow_settings_builder.cli_overrides(cli);
    }
    if !workflow_run_layer.eq(&RunLayer::default()) {
        workflow_settings_builder =
            workflow_settings_builder.workflow_run_layer(workflow_run_layer);
    }
    for config in manifest
        .configs
        .iter()
        .filter(|config| config.type_ == types::ManifestConfigType::Project)
    {
        if let Some(source) = config.source.as_deref() {
            workflow_settings_builder = workflow_settings_builder.project_toml(source)?;
        }
    }
    for config in manifest
        .configs
        .iter()
        .filter(|config| config.type_ == types::ManifestConfigType::User)
    {
        if let Some(source) = config.source.as_deref() {
            workflow_settings_builder = workflow_settings_builder.user_toml(source)?;
        }
    }
    let mut settings = workflow_settings_builder
        .build()
        .context("failed to resolve manifest settings")?;
    if let Some(goal) = manifest.goal.as_ref() {
        settings.run.goal = Some(RunGoal::Inline(InterpString::parse(&goal.text)));
    }

    let in_place = settings.run.sandbox.provider == "local"
        && settings.run.sandbox.local.worktree_mode == WorktreeMode::Never;

    Ok(PreparedManifest {
        cwd: cwd.clone(),
        git: manifest.git.clone(),
        root_source,
        run_id: manifest
            .run_id
            .as_deref()
            .map(str::parse::<RunId>)
            .transpose()
            .context("invalid run ID")?,
        settings: settings.clone(),
        target_path,
        workflow_bundle,
        workflow_input,
        source_directory: resolve_working_directory(&settings, &cwd),
        in_place,
    })
}

pub(crate) fn validate_prepared_manifest(
    prepared: &PreparedManifest,
) -> Result<Validated, WorkflowError> {
    validate(ValidateInput {
        workflow:          WorkflowInput::Bundled(prepared.workflow_input.clone()),
        settings:          prepared.settings.clone(),
        cwd:               prepared.cwd.clone(),
        custom_transforms: Vec::new(),
    })
}

pub(crate) fn create_run_input(
    prepared: PreparedManifest,
    configured_providers: Vec<Provider>,
    web_url: Option<String>,
) -> CreateRunInput {
    CreateRunInput {
        workflow: WorkflowInput::Bundled(prepared.workflow_input),
        settings: prepared.settings,
        cwd: prepared.cwd,
        workflow_slug: None,
        workflow_path: Some(prepared.target_path),
        workflow_bundle: Some(prepared.workflow_bundle),
        submitted_manifest_bytes: None,
        run_id: prepared.run_id,
        git: prepared.git,
        fork_source_ref: None,
        in_place: prepared.in_place,
        provenance: None,
        configured_providers,
        web_url,
    }
}

pub(crate) async fn run_preflight(
    state: &AppState,
    prepared: &PreparedManifest,
    validated: &Validated,
) -> Result<(types::PreflightResponse, bool)> {
    let (report, checks_ok) = build_preflight_report(state, prepared, validated).await?;
    let preflight_ok = !validated.has_errors() && checks_ok;
    Ok((
        preflight_response(
            validated,
            prepared.target_path.as_path(),
            &report,
            preflight_ok,
        ),
        preflight_ok,
    ))
}

pub(crate) fn validate_response(
    prepared: &PreparedManifest,
    validated: &Validated,
) -> types::ValidateResponse {
    types::ValidateResponse {
        ok:       !validated.has_errors(),
        workflow: workflow_summary(validated, prepared.target_path.as_path()),
    }
}

pub(crate) fn graph_source(prepared: &PreparedManifest, direction: Option<&str>) -> String {
    direction.map_or_else(
        || prepared.root_source.clone(),
        |direction| apply_direction(&prepared.root_source, direction).into_owned(),
    )
}

pub fn workflow_bundle_from_manifest(
    workflows: &HashMap<String, types::ManifestWorkflow>,
) -> Result<WorkflowBundle> {
    let mut bundled = HashMap::new();
    let mut workflow_wire_keys = HashMap::new();

    for (wire_key, workflow) in workflows {
        let path = ManifestPath::from_wire(wire_key)
            .ok_or_else(|| anyhow!("invalid manifest workflow key: {wire_key}"))?;
        if let Some(previous) = workflow_wire_keys.get(&path) {
            bail!(
                "duplicate canonical workflow key: {path} (from wire keys {previous:?} and \
                 {wire_key:?})"
            );
        }
        workflow_wire_keys.insert(path.clone(), wire_key.clone());

        let files = workflow_files_from_manifest(&workflow.files)?;
        let config = workflow
            .config
            .as_ref()
            .map(|config| {
                let path = ManifestPath::from_wire(&config.path).ok_or_else(|| {
                    anyhow!("invalid manifest workflow config path: {}", config.path)
                })?;
                Ok::<_, anyhow::Error>(ParsedWorkflowConfig {
                    path,
                    source: config.source.clone(),
                })
            })
            .transpose()?;

        bundled.insert(path.clone(), BundledWorkflow {
            path,
            source: workflow.source.clone(),
            config,
            files,
        });
    }

    Ok(WorkflowBundle::new(bundled))
}

fn workflow_files_from_manifest(
    files: &HashMap<String, types::ManifestFileEntry>,
) -> Result<HashMap<ManifestPath, String>> {
    let mut bundled = HashMap::new();
    let mut file_wire_keys = HashMap::new();

    for (wire_key, entry) in files {
        let path = ManifestPath::from_wire(wire_key)
            .ok_or_else(|| anyhow!("invalid manifest file key: {wire_key}"))?;
        if let Some(previous) = file_wire_keys.get(&path) {
            bail!(
                "duplicate canonical file key: {path} (from wire keys {previous:?} and \
                 {wire_key:?})"
            );
        }
        if let Some(from) = entry.ref_.from.as_deref() {
            ManifestPath::from_wire(from)
                .ok_or_else(|| anyhow!("invalid manifest file ref from: {from}"))?;
        }
        file_wire_keys.insert(path.clone(), wire_key.clone());
        bundled.insert(path, entry.content.clone());
    }

    Ok(bundled)
}

fn root_workflow_run_layer(workflow: &BundledWorkflow) -> Result<RunLayer> {
    let Some(config) = workflow.config.as_ref() else {
        return Ok(RunLayer::default());
    };

    let mut document: toml::Table = config
        .source
        .parse()
        .context("Failed to parse run config TOML")?;
    let mut run = document
        .remove("run")
        .map(toml::Value::try_into::<RunLayer>)
        .transpose()
        .context("Failed to parse run config TOML")?
        .unwrap_or_default();
    resolve_manifest_dockerfile(&mut run, &config.path, &workflow.files)?;
    Ok(run)
}

fn manifest_args_overrides(args: Option<&types::ManifestArgs>) -> ManifestSettingsOverrides {
    let Some(args) = args else {
        return ManifestSettingsOverrides::default();
    };

    let model = (args.model.is_some() || args.provider.is_some()).then(|| RunModelLayer {
        provider:  args.provider.as_deref().map(InterpString::parse),
        name:      args.model.as_deref().map(InterpString::parse),
        fallbacks: Vec::new(),
    });
    let local_worktree = args
        .worktree_mode
        .as_deref()
        .and_then(parse_worktree_mode_arg)
        .map(|mode| LocalSandboxLayer {
            worktree_mode: Some(mode),
        });
    let sandbox = (args.sandbox.is_some()
        || args.preserve_sandbox.is_some()
        || args.docker_image.is_some()
        || local_worktree.is_some())
    .then(|| RunSandboxLayer {
        provider: args.sandbox.clone(),
        preserve: args.preserve_sandbox,
        local: local_worktree,
        docker: args.docker_image.as_ref().map(|image| DockerSandboxLayer {
            image: Some(image.clone()),
            ..DockerSandboxLayer::default()
        }),
        ..RunSandboxLayer::default()
    });

    let execution_has_any =
        args.dry_run.is_some() || args.auto_approve.is_some() || args.no_retro.is_some();
    let execution = execution_has_any.then(|| RunExecutionLayer {
        mode:     args
            .dry_run
            .map(|d| if d { RunMode::DryRun } else { RunMode::Normal }),
        approval: args.auto_approve.map(|a| {
            if a {
                ApprovalMode::Auto
            } else {
                ApprovalMode::Prompt
            }
        }),
        retros:   args.no_retro.map(|nr| !nr),
    });

    let run_has_any =
        model.is_some() || sandbox.is_some() || execution.is_some() || !args.label.is_empty();

    let run = run_has_any.then(|| RunLayer {
        model,
        sandbox,
        execution,
        metadata: ReplaceMap::from(parse_labels(&args.label)),
        ..RunLayer::default()
    });

    // Verbose is a CLI output concern in v2; route it through cli.output.verbosity.
    let cli = args.verbose.and_then(|verbose| {
        verbose.then(|| CliLayer {
            output: Some(CliOutputLayer {
                verbosity: Some(OutputVerbosity::Verbose),
                ..CliOutputLayer::default()
            }),
            ..CliLayer::default()
        })
    });

    ManifestSettingsOverrides { run, cli }
}

fn parse_worktree_mode_arg(value: &str) -> Option<WorktreeMode> {
    match value {
        "always" => Some(WorktreeMode::Always),
        "clean" => Some(WorktreeMode::Clean),
        "dirty" => Some(WorktreeMode::Dirty),
        "never" => Some(WorktreeMode::Never),
        _ => None,
    }
}

fn parse_labels(labels: &[String]) -> HashMap<String, String> {
    labels
        .iter()
        .filter_map(|label| label.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn resolve_working_directory(settings: &WorkflowSettings, caller_cwd: &Path) -> PathBuf {
    let Some(work_dir) = settings
        .run
        .working_dir
        .as_ref()
        .map(InterpString::as_source)
    else {
        return caller_cwd.to_path_buf();
    };
    let path = PathBuf::from(&work_dir);
    if path.is_absolute() {
        path
    } else {
        caller_cwd.join(path)
    }
}

fn resolve_interp(value: &InterpString) -> String {
    value
        .resolve(process_env_var)
        .map_or_else(|_| value.as_source(), |resolved| resolved.value)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Manifest preflight interpolation owns a process-env lookup facade for {{ env.* }} values."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn resolve_manifest_dockerfile(
    run: &mut RunLayer,
    config_path: &ManifestPath,
    files: &HashMap<ManifestPath, String>,
) -> Result<()> {
    let source = run
        .sandbox
        .as_mut()
        .and_then(|sandbox| sandbox.daytona.as_mut())
        .and_then(|daytona| daytona.snapshot.as_mut())
        .and_then(|snapshot| snapshot.dockerfile.as_mut());
    let Some(source) = source else {
        return Ok(());
    };
    let DaytonaDockerfileLayer::Path { path } = &*source else {
        return Ok(());
    };
    let path_owned = path.clone();
    let manifest_path = ManifestPath::from_reference(config_path.parent_or_dot(), &path_owned)
        .ok_or_else(|| anyhow!("unsupported dockerfile reference: {path_owned}"))?;
    let content = files
        .get(&manifest_path)
        .cloned()
        .ok_or_else(|| anyhow!("missing bundled dockerfile: {manifest_path}"))?;
    *source = DaytonaDockerfileLayer::Inline(content);
    Ok(())
}

async fn build_preflight_report(
    state: &AppState,
    prepared: &PreparedManifest,
    validated: &Validated,
) -> Result<(CheckReport, bool)> {
    let graph = validated.graph();
    let mut checks = base_preflight_checks(prepared, graph);
    if validated.has_errors() {
        return Ok((
            CheckReport {
                title:    "Run Preflight".into(),
                sections: vec![CheckSection {
                    title: String::new(),
                    checks,
                }],
            },
            true,
        ));
    }

    let configured_providers = state.llm_source.configured_providers().await;
    let materialized = materialize_run(
        prepared.settings.clone(),
        graph,
        Catalog::builtin(),
        &configured_providers,
    );
    let resolved_run = materialized.run;
    let server_settings = state.server_settings();
    let github_integration = &server_settings.server.integrations.github;
    let sandbox_provider = resolve_sandbox_provider(&resolved_run)?;
    let sandbox_provider =
        if resolved_run.execution.mode == RunMode::DryRun && !sandbox_provider.is_local() {
            SandboxProvider::Local
        } else {
            sandbox_provider
        };
    let needs_github_credentials =
        sandbox_provider.is_clone_based() || !github_integration.permissions.is_empty();
    let github_app = if needs_github_credentials {
        state
            .github_credentials(github_integration)
            .unwrap_or_default()
    } else {
        None
    };

    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    let sandbox_ok = run_sandbox_check(
        &mut checks,
        sandbox_provider,
        prepared,
        &resolved_run,
        github_app.clone(),
        daytona_api_key,
    )
    .await;
    let repository_access_ok = run_repository_access_check(
        &mut checks,
        sandbox_provider,
        prepared,
        &resolved_run,
        github_app.clone(),
    )
    .await;
    let llm_ok = run_llm_check(
        state,
        &mut checks,
        graph,
        &resolved_run,
        &configured_providers,
    )
    .await;
    run_github_token_check(&mut checks, prepared, &server_settings.server, github_app).await;

    let checks_ok = sandbox_ok && repository_access_ok && llm_ok;

    Ok((
        CheckReport {
            title:    "Run Preflight".into(),
            sections: vec![CheckSection {
                title: String::new(),
                checks,
            }],
        },
        checks_ok,
    ))
}

fn base_preflight_checks(prepared: &PreparedManifest, graph: &Graph) -> Vec<CheckResult> {
    let setup_command_count = prepared.settings.run.prepare.commands.len();
    let repo_summary = prepared.git.as_ref().map_or_else(
        || "unknown".to_string(),
        |git| {
            let https = fabro_github::ssh_url_to_https(&git.origin_url);
            fabro_github::parse_github_owner_repo(&https).map_or_else(
                |_| git.origin_url.clone(),
                |(owner, repo)| format!("{owner}/{repo}"),
            )
        },
    );

    vec![
        CheckResult {
            name:        "Repository".into(),
            status:      CheckStatus::Pass,
            summary:     repo_summary,
            details:     vec![
                CheckDetail::new(format!("Setup commands: {setup_command_count}")),
                CheckDetail {
                    text: format!(
                        "Git: {}",
                        prepared
                            .git
                            .as_ref()
                            .map_or("unknown", |git| match git.dirty {
                                fabro_types::DirtyStatus::Clean => "clean",
                                fabro_types::DirtyStatus::Dirty => "dirty",
                                fabro_types::DirtyStatus::Unknown => "unknown",
                            })
                    ),
                    warn: prepared
                        .git
                        .as_ref()
                        .is_some_and(|git| git.dirty != fabro_types::DirtyStatus::Clean),
                },
            ],
            remediation: None,
        },
        CheckResult {
            name:        "Workflow".into(),
            status:      CheckStatus::Pass,
            summary:     graph.name.clone(),
            details:     vec![
                CheckDetail::new(format!("Nodes: {}", graph.nodes.len())),
                CheckDetail::new(format!("Edges: {}", graph.edges.len())),
                CheckDetail::new(format!("Goal: {}", graph.goal())),
            ],
            remediation: None,
        },
    ]
}

fn resolve_sandbox_provider(settings: &RunNamespace) -> Result<SandboxProvider> {
    Ok(Some(str::parse::<SandboxProvider>(
        settings.sandbox.provider.as_str(),
    ))
    .transpose()
    .context("Invalid sandbox provider")?
    .unwrap_or_default())
}

fn resolve_daytona_config(settings: &RunNamespace) -> Option<DaytonaConfig> {
    settings
        .sandbox
        .daytona
        .as_ref()
        .map(runtime_daytona_config)
}

fn resolve_docker_config(settings: &RunNamespace) -> Option<DockerSandboxOptions> {
    settings.sandbox.docker.as_ref().map(runtime_docker_config)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitRemoteRefCheck {
    origin_url: String,
    branch:     Option<String>,
}

fn clone_disabled_for_provider(provider: SandboxProvider, resolved_run: &RunNamespace) -> bool {
    match provider {
        SandboxProvider::Docker => resolved_run
            .sandbox
            .docker
            .as_ref()
            .is_some_and(|docker| docker.skip_clone),
        SandboxProvider::Daytona => resolved_run
            .sandbox
            .daytona
            .as_ref()
            .is_some_and(|daytona| daytona.skip_clone),
        SandboxProvider::Local => false,
    }
}

fn repository_access_details(request: &GitRemoteRefCheck) -> Vec<CheckDetail> {
    let mut details = vec![CheckDetail::new(format!("Origin: {}", request.origin_url))];
    if let Some(branch) = request.branch.as_ref() {
        details.push(CheckDetail::new(format!("Branch: {branch}")));
    }
    details
}

async fn run_repository_access_check(
    checks: &mut Vec<CheckResult>,
    sandbox_provider: SandboxProvider,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
) -> bool {
    run_repository_access_check_with(
        checks,
        sandbox_provider,
        prepared,
        resolved_run,
        github_app,
        check_git_remote_ref,
    )
    .await
}

async fn run_repository_access_check_with<F, Fut>(
    checks: &mut Vec<CheckResult>,
    sandbox_provider: SandboxProvider,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
    check_remote_ref: F,
) -> bool
where
    F: FnOnce(GitRemoteRefCheck, Option<fabro_github::GitHubCredentials>) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    if !sandbox_provider.is_clone_based()
        || clone_disabled_for_provider(sandbox_provider, resolved_run)
    {
        return true;
    }

    let Some(git) = prepared.git.as_ref() else {
        return true;
    };

    let origin_url = fabro_github::normalize_repo_origin_url(&git.origin_url);
    if let Err(err) = fabro_github::parse_github_owner_repo(&origin_url) {
        checks.push(CheckResult {
            name:        "Repository Access".into(),
            status:      CheckStatus::Error,
            summary:     "failed".into(),
            details:     vec![CheckDetail::new(format!("Origin: {origin_url}"))],
            remediation: Some(format!(
                "Clone-based sandboxes currently support GitHub repository origins only: {err}"
            )),
        });
        return false;
    }

    let request = GitRemoteRefCheck {
        origin_url,
        branch: Some(git.branch.clone()).filter(|branch| !branch.trim().is_empty()),
    };
    let details = repository_access_details(&request);

    match check_remote_ref(request, github_app).await {
        Ok(()) => {
            checks.push(CheckResult {
                name: "Repository Access".into(),
                status: CheckStatus::Pass,
                summary: "reachable".into(),
                details,
                remediation: None,
            });
            true
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "Repository Access".into(),
                status: CheckStatus::Error,
                summary: "failed".into(),
                details,
                remediation: Some(format!("Failed to verify repository access: {err}")),
            });
            false
        }
    }
}

async fn check_git_remote_ref(
    request: GitRemoteRefCheck,
    github_app: Option<fabro_github::GitHubCredentials>,
) -> Result<(), String> {
    let auth_url = match github_app.as_ref() {
        Some(creds) => Some(
            fabro_github::resolve_authenticated_url(
                &fabro_github::GitHubContext::new(creds, &fabro_github::github_api_base_url()),
                &request.origin_url,
            )
            .await
            .map_err(|err| format!("Failed to resolve GitHub credentials: {err}"))?,
        ),
        None => None,
    };
    let remote_url = auth_url
        .as_ref()
        .map_or(request.origin_url.as_str(), |url| url.as_raw_url().as_str());

    let mut command = Command::new("git");
    command.env("GIT_TERMINAL_PROMPT", "0").args([
        "ls-remote",
        "--heads",
        "--exit-code",
        remote_url,
    ]);
    if let Some(branch) = request.branch.as_ref() {
        command.arg(branch);
    }

    let output = time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| "git ls-remote timed out after 10s".to_string())?
        .map_err(|err| format!("Failed to run git ls-remote: {err}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("git ls-remote exited with status {}", output.status)
    };
    Err(redact_auth_url(&message, auth_url.as_ref()))
}

fn preflight_sandbox_spec(
    sandbox_provider: SandboxProvider,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
    daytona_api_key: Option<String>,
) -> SandboxSpec {
    let clone_origin_url = prepared
        .git
        .as_ref()
        .map(|git| fabro_github::normalize_repo_origin_url(&git.origin_url));
    let clone_branch = prepared.git.as_ref().map(|git| git.branch.clone());

    match sandbox_provider {
        SandboxProvider::Local => SandboxSpec::Local {
            working_directory: prepared.source_directory.clone(),
        },
        SandboxProvider::Docker => {
            let mut config = resolve_docker_config(resolved_run).unwrap_or_default();
            config.skip_clone = true;
            SandboxSpec::Docker {
                config,
                github_app,
                run_id: None,
                clone_origin_url,
                clone_branch,
            }
        }
        SandboxProvider::Daytona => {
            let mut config = resolve_daytona_config(resolved_run).unwrap_or_default();
            config.skip_clone = true;
            SandboxSpec::Daytona {
                config: Box::new(config),
                github_app,
                run_id: None,
                clone_origin_url,
                clone_branch,
                api_key: daytona_api_key,
            }
        }
    }
}

async fn run_sandbox_check(
    checks: &mut Vec<CheckResult>,
    sandbox_provider: SandboxProvider,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
    daytona_api_key: Option<String>,
) -> bool {
    let spec = preflight_sandbox_spec(
        sandbox_provider,
        prepared,
        resolved_run,
        github_app.clone(),
        daytona_api_key,
    );
    let sandbox_result: Result<Arc<dyn Sandbox>, String> = spec.build(None).await.map_err(|err| {
        if matches!(sandbox_provider, SandboxProvider::Daytona) {
            format!("Daytona sandbox creation failed: {err}")
        } else {
            err.to_string()
        }
    });

    match sandbox_result {
        Ok(sandbox) => match sandbox.initialize().await {
            Ok(()) => {
                let mut details = vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))];
                if sandbox_provider.is_clone_based()
                    && prepared.git.is_none()
                    && !clone_disabled_for_provider(sandbox_provider, resolved_run)
                {
                    details.push(CheckDetail {
                        text: "No clone source present; sandbox workspace will be empty".into(),
                        warn: true,
                    });
                }
                if let Err(err) = sandbox.cleanup().await {
                    checks.push(CheckResult {
                        name: "Sandbox".into(),
                        status: CheckStatus::Error,
                        summary: "cleanup failed".into(),
                        details,
                        remediation: Some(format!("Sandbox cleanup failed: {err}")),
                    });
                    return false;
                }
                checks.push(CheckResult {
                    name: "Sandbox".into(),
                    status: CheckStatus::Pass,
                    summary: sandbox_provider.to_string(),
                    details,
                    remediation: None,
                });
                true
            }
            Err(err) => {
                let cleanup_error = sandbox.cleanup().await.err();
                checks.push(CheckResult {
                    name:        "Sandbox".into(),
                    status:      CheckStatus::Error,
                    summary:     "failed".into(),
                    details:     vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                    remediation: Some(cleanup_error.map_or_else(
                        || format!("Sandbox init failed: {err}"),
                        |cleanup| {
                            format!("Sandbox init failed: {err}; cleanup also failed: {cleanup}")
                        },
                    )),
                });
                false
            }
        },
        Err(err) => {
            checks.push(CheckResult {
                name:        "Sandbox".into(),
                status:      CheckStatus::Error,
                summary:     "failed".into(),
                details:     vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                remediation: Some(err),
            });
            false
        }
    }
}

async fn run_llm_check(
    state: &AppState,
    checks: &mut Vec<CheckResult>,
    graph: &Graph,
    settings: &RunNamespace,
    configured_providers: &[Provider],
) -> bool {
    let (model, provider) = resolve_model_provider(settings, graph, configured_providers);
    let default_provider = provider.as_deref().unwrap_or("anthropic");

    match state.resolve_llm_client().await {
        Ok(result) => {
            let configured = result
                .client
                .provider_names()
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>();
            let auth_issues = result.auth_issues;
            let client = Arc::new(result.client);
            let mut model_providers = std::collections::BTreeSet::new();
            let mut has_llm_nodes = false;

            for node in graph.nodes.values() {
                if !is_llm_handler_type(node.handler_type()) {
                    continue;
                }
                has_llm_nodes = true;
                let node_model = node.model().unwrap_or(&model);
                let node_provider = node.provider().unwrap_or(default_provider);
                let (resolved_model, resolved_provider) =
                    if let Some(info) = Catalog::builtin().get(node_model) {
                        (info.id.clone(), info.provider.to_string())
                    } else {
                        (node_model.to_string(), node_provider.to_string())
                    };
                let final_provider = if node.provider().is_some() {
                    node_provider.to_string()
                } else {
                    resolved_provider
                };
                model_providers.insert((resolved_model, final_provider));
            }

            if !has_llm_nodes {
                return true;
            }

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
                    Ok(provider) => {
                        let mut status = CheckStatus::Pass;
                        let remediation = if let Some((_, issue)) = auth_issues
                            .iter()
                            .find(|(candidate, _)| *candidate == provider)
                        {
                            status = CheckStatus::Warning;
                            all_ok = false;
                            Some(auth_issue_message(provider, issue))
                        } else if !configured.iter().any(|name| name == provider_name) {
                            status = CheckStatus::Warning;
                            all_ok = false;
                            Some(format!("Provider \"{provider_name}\" is not configured"))
                        } else {
                            let probe_model = preflight_probe_model(model_id, provider);
                            let outcome = run_model_test(
                                &probe_model,
                                ModelTestMode::Basic,
                                Arc::clone(&client),
                            )
                            .await;
                            if outcome.status == ModelTestStatus::Ok {
                                None
                            } else {
                                status = CheckStatus::Error;
                                all_ok = false;
                                Some(format!(
                                    "Model availability probe failed: {}",
                                    outcome
                                        .error_message
                                        .unwrap_or_else(|| "unknown error".to_string())
                                ))
                            }
                        };
                        checks.push(CheckResult {
                            name: "LLM".into(),
                            status,
                            summary: model_id.clone(),
                            details: vec![
                                CheckDetail::new(format!("Provider: {provider_name}")),
                                CheckDetail::new("Probe: basic generation".to_string()),
                            ],
                            remediation,
                        });
                    }
                    Err(err) => {
                        checks.push(CheckResult {
                            name:        "LLM".into(),
                            status:      CheckStatus::Error,
                            summary:     model_id.clone(),
                            details:     vec![CheckDetail::new(format!(
                                "Provider: {provider_name}"
                            ))],
                            remediation: Some(format!(
                                "Invalid provider \"{provider_name}\": {err}"
                            )),
                        });
                        all_ok = false;
                    }
                }
            }
            all_ok
        }
        Err(err) => {
            checks.push(CheckResult {
                name:        "LLM".into(),
                status:      CheckStatus::Error,
                summary:     "initialization failed".into(),
                details:     vec![],
                remediation: Some(format!("LLM client init failed: {err}")),
            });
            false
        }
    }
}

fn preflight_probe_model(model_id: &str, provider: Provider) -> Model {
    if let Some(info) = Catalog::builtin().get(model_id) {
        let mut model = info.clone();
        model.provider = provider;
        return model;
    }

    Model {
        id: model_id.to_string(),
        provider,
        family: "custom".to_string(),
        display_name: model_id.to_string(),
        limits: ModelLimits {
            context_window: 0,
            max_output:     None,
        },
        training: None,
        knowledge_cutoff: None,
        features: ModelFeatures {
            tools:     false,
            vision:    false,
            reasoning: false,
            effort:    false,
        },
        costs: ModelCosts {
            input_cost_per_mtok:       None,
            output_cost_per_mtok:      None,
            cache_input_cost_per_mtok: None,
        },
        estimated_output_tps: None,
        aliases: Vec::new(),
        default: false,
        configured: true,
    }
}

fn resolve_model_provider(
    settings: &RunNamespace,
    _graph: &Graph,
    configured_providers: &[Provider],
) -> (String, Option<String>) {
    let provider = settings
        .model
        .provider
        .as_ref()
        .map(InterpString::as_source);
    let model = settings.model.name.as_ref().map_or_else(
        || {
            Catalog::builtin()
                .default_for_configured(configured_providers)
                .id
                .clone()
        },
        InterpString::as_source,
    );

    match Catalog::builtin().get(&model) {
        Some(info) => (
            info.id.clone(),
            provider.or(Some(info.provider.to_string())),
        ),
        None => (model, provider),
    }
}

fn runtime_daytona_config(settings: &DaytonaSettings) -> DaytonaConfig {
    DaytonaConfig {
        auto_stop_interval: settings.auto_stop_interval,
        labels:             (!settings.labels.is_empty()).then_some(settings.labels.clone()),
        snapshot:           settings
            .snapshot
            .as_ref()
            .map(|snapshot| DaytonaSnapshotSettings {
                name:       snapshot.name.clone(),
                cpu:        snapshot.cpu,
                memory:     snapshot.memory_gb,
                disk:       snapshot.disk_gb,
                dockerfile: snapshot
                    .dockerfile
                    .as_ref()
                    .map(|dockerfile| match dockerfile {
                        DockerfileSource::Inline(text) => {
                            SandboxDockerfileSource::Inline(text.clone())
                        }
                        DockerfileSource::Path { path } => {
                            SandboxDockerfileSource::Path { path: path.clone() }
                        }
                    }),
            }),
        network:            settings.network.as_ref().map(|network| match network {
            DaytonaNetworkLayer::Block => DaytonaNetwork::Block,
            DaytonaNetworkLayer::AllowAll => DaytonaNetwork::AllowAll,
            DaytonaNetworkLayer::AllowList { allow_list } => {
                DaytonaNetwork::AllowList(allow_list.clone())
            }
        }),
        skip_clone:         settings.skip_clone,
    }
}

fn runtime_docker_config(settings: &DockerSettings) -> DockerSandboxOptions {
    let mut env_vars = settings
        .env_vars
        .iter()
        .map(|(key, value)| format!("{key}={}", resolve_interp(value)))
        .collect::<Vec<_>>();
    env_vars.sort();

    DockerSandboxOptions {
        image: settings.image.clone(),
        network_mode: settings.network_mode.clone(),
        memory_limit: settings.memory_limit,
        cpu_quota: settings.cpu_quota,
        env_vars,
        skip_clone: settings.skip_clone,
        ..DockerSandboxOptions::default()
    }
}

async fn run_github_token_check(
    checks: &mut Vec<CheckResult>,
    prepared: &PreparedManifest,
    settings: &ServerNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
) {
    if settings.integrations.github.permissions.is_empty() {
        return;
    }

    // Resolve InterpString permission values eagerly for token minting and
    // for display in the preflight report.
    let github_permissions: HashMap<String, String> = settings
        .integrations
        .github
        .permissions
        .iter()
        .map(|(k, v)| (k.clone(), v.as_source()))
        .collect();

    let perm_details = github_permissions
        .iter()
        .map(|(key, value)| CheckDetail::new(format!("{key}: {value}")))
        .collect::<Vec<_>>();
    match (&github_app, prepared.git.as_ref()) {
        (Some(creds), Some(git)) => {
            match mint_github_token(creds, &git.origin_url, &github_permissions).await {
                Ok(_) => checks.push(CheckResult {
                    name:        "GitHub Token".into(),
                    status:      CheckStatus::Pass,
                    summary:     "minted".into(),
                    details:     perm_details,
                    remediation: None,
                }),
                Err(err) => checks.push(CheckResult {
                    name:        "GitHub Token".into(),
                    status:      CheckStatus::Error,
                    summary:     "failed".into(),
                    details:     perm_details,
                    remediation: Some(format!("Failed to mint GitHub token: {err}")),
                }),
            }
        }
        _ => checks.push(CheckResult {
            name:        "GitHub Token".into(),
            status:      CheckStatus::Warning,
            summary:     "skipped".into(),
            details:     vec![],
            remediation: Some("No GitHub credentials or origin URL available".to_string()),
        }),
    }
}

async fn mint_github_token(
    creds: &fabro_github::GitHubCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> Result<String> {
    if let fabro_github::GitHubCredentials::Token(token) = creds {
        return Ok(token.clone());
    }

    let https_url = fabro_github::ssh_url_to_https(origin_url);
    let (owner, repo) = fabro_github::parse_github_owner_repo(&https_url)?;
    let fabro_github::GitHubCredentials::App(creds) = creds else {
        unreachable!("token credentials return early");
    };
    let jwt = fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = fabro_http::http_client()?;
    let perms_json = serde_json::to_value(permissions)?;
    let install_url = creds.installation_url(&owner);
    fabro_github::create_installation_access_token_with_permissions_and_install_url(
        &client,
        &jwt,
        &owner,
        &repo,
        &fabro_github::github_api_base_url(),
        perms_json,
        install_url.as_deref(),
    )
    .await
}

fn preflight_response(
    validated: &Validated,
    target_path: &Path,
    report: &CheckReport,
    ok: bool,
) -> types::PreflightResponse {
    types::PreflightResponse {
        ok,
        checks: report_to_api(report),
        workflow: workflow_summary(validated, target_path),
    }
}

fn workflow_summary(validated: &Validated, target_path: &Path) -> types::PreflightWorkflowSummary {
    types::PreflightWorkflowSummary {
        diagnostics: diagnostics_to_api(validated.diagnostics()),
        edges:       i64::try_from(validated.graph().edges.len())
            .expect("graph edge count should fit in i64"),
        goal:        validated.graph().goal().to_string(),
        graph_path:  Some(target_path.display().to_string()),
        name:        validated.graph().name.clone(),
        nodes:       i64::try_from(validated.graph().nodes.len())
            .expect("graph node count should fit in i64"),
    }
}

fn diagnostics_to_api(
    diagnostics: &[fabro_validate::Diagnostic],
) -> Vec<types::WorkflowDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| types::WorkflowDiagnostic {
            edge:     diagnostic
                .edge
                .as_ref()
                .map(|edge: &(String, String)| [edge.0.clone(), edge.1.clone()]),
            fix:      diagnostic.fix.clone(),
            message:  diagnostic.message.clone(),
            node_id:  diagnostic.node_id.clone(),
            rule:     diagnostic.rule.clone(),
            severity: match diagnostic.severity {
                Severity::Error => types::WorkflowDiagnosticSeverity::Error,
                Severity::Warning => types::WorkflowDiagnosticSeverity::Warning,
                Severity::Info => types::WorkflowDiagnosticSeverity::Info,
            },
        })
        .collect()
}

fn report_to_api(report: &CheckReport) -> types::PreflightCheckReport {
    types::PreflightCheckReport {
        sections: report
            .sections
            .iter()
            .map(|section| types::PreflightCheckSection {
                checks: section
                    .checks
                    .iter()
                    .map(|check| types::PreflightCheckResult {
                        details:     check
                            .details
                            .iter()
                            .map(|detail| types::PreflightCheckDetail {
                                text: detail.text.clone(),
                                warn: detail.warn,
                            })
                            .collect(),
                        name:        check.name.clone(),
                        remediation: check.remediation.clone(),
                        status:      match check.status {
                            CheckStatus::Pass => types::PreflightCheckResultStatus::Pass,
                            CheckStatus::Warning => types::PreflightCheckResultStatus::Warning,
                            CheckStatus::Error => types::PreflightCheckResultStatus::Error,
                        },
                        summary:     check.summary.clone(),
                    })
                    .collect(),
                title:  section.title.clone(),
            })
            .collect(),
        title:    report.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_manifest() -> types::RunManifest {
        types::RunManifest {
            args:      None,
            configs:   Vec::new(),
            cwd:       "/tmp/project".to_string(),
            git:       None,
            goal:      None,
            run_id:    None,
            target:    types::ManifestTarget {
                identifier: "workflow.fabro".to_string(),
                path:       "workflow.fabro".to_string(),
            },
            version:   1,
            workflows: HashMap::from([("workflow.fabro".to_string(), types::ManifestWorkflow {
                config: None,
                files:  HashMap::new(),
                source:
                    "digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }"
                        .to_string(),
            })]),
        }
    }

    fn invalid_manifest() -> types::RunManifest {
        types::RunManifest {
            workflows: HashMap::from([("workflow.fabro".to_string(), types::ManifestWorkflow {
                config: None,
                files:  HashMap::new(),
                source: "digraph Invalid { exit [shape=Msquare] orphan exit -> orphan }"
                    .to_string(),
            })]),
            ..minimal_manifest()
        }
    }

    fn server_settings_fixture(source: &str) -> RunLayer {
        let mut document: toml::Table = source.parse().expect("v2 fixture should parse");
        document
            .remove("run")
            .map(toml::Value::try_into::<RunLayer>)
            .transpose()
            .expect("run settings should parse")
            .unwrap_or_default()
    }

    fn default_settings_fixture() -> RunLayer {
        RunLayer::default()
    }

    fn manifest_workflow() -> types::ManifestWorkflow {
        types::ManifestWorkflow {
            config: None,
            files:  HashMap::new(),
            source: "digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }"
                .to_string(),
        }
    }

    fn manifest_file(content: &str) -> types::ManifestFileEntry {
        types::ManifestFileEntry {
            content: content.to_string(),
            ref_:    types::ManifestFileRef {
                from:     Some("workflow.fabro".to_string()),
                original: "prompt.md".to_string(),
                type_:    types::ManifestFileRefType::FileInline,
            },
        }
    }

    fn git_context(origin_url: &str, branch: &str) -> types::GitContext {
        types::GitContext {
            origin_url:   origin_url.to_string(),
            branch:       branch.to_string(),
            sha:          None,
            dirty:        fabro_types::DirtyStatus::Clean,
            push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
        }
    }

    fn prepared_and_resolved_for_sandbox(
        provider: SandboxProvider,
        skip_clone: bool,
        git: Option<types::GitContext>,
    ) -> (PreparedManifest, RunNamespace) {
        let mut manifest = minimal_manifest();
        manifest.git = git;
        manifest.configs.push(types::ManifestConfig {
            path:   Some("/tmp/project/.fabro/project.toml".to_string()),
            source: Some(format!(
                r#"
_version = 1

[run.sandbox]
provider = "{provider}"

[run.sandbox.{provider}]
skip_clone = {skip_clone}
"#
            )),
            type_:  types::ManifestConfigType::Project,
        });

        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_prepared_manifest(&prepared).unwrap();
        let resolved = materialize_run(
            prepared.settings.clone(),
            validated.graph(),
            Catalog::builtin(),
            &[Provider::Anthropic],
        )
        .run;

        (prepared, resolved)
    }

    #[tokio::test]
    async fn repository_access_check_skips_when_clone_is_disabled() {
        let (prepared, resolved) = prepared_and_resolved_for_sandbox(
            SandboxProvider::Docker,
            true,
            Some(git_context("https://github.com/acme/widgets", "main")),
        );
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_check = Arc::clone(&calls);
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            SandboxProvider::Docker,
            &prepared,
            &resolved,
            None,
            move |request, _github_app| {
                calls_for_check.lock().unwrap().push(request);
                async { Ok(()) }
            },
        )
        .await;

        assert!(ok);
        assert!(checks.is_empty());
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn repository_access_check_rejects_non_github_origins_before_remote_probe() {
        let (prepared, resolved) = prepared_and_resolved_for_sandbox(
            SandboxProvider::Docker,
            false,
            Some(git_context("https://gitlab.com/acme/widgets", "main")),
        );
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_check = Arc::clone(&calls);
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            SandboxProvider::Docker,
            &prepared,
            &resolved,
            None,
            move |request, _github_app| {
                calls_for_check.lock().unwrap().push(request);
                async { Ok(()) }
            },
        )
        .await;

        assert!(!ok);
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "Repository Access");
        assert_eq!(checks[0].status, CheckStatus::Error);
        assert!(
            checks[0]
                .remediation
                .as_deref()
                .unwrap_or_default()
                .contains("GitHub repository origins only")
        );
    }

    #[tokio::test]
    async fn repository_access_check_probes_normalized_github_branch() {
        let (prepared, resolved) = prepared_and_resolved_for_sandbox(
            SandboxProvider::Docker,
            false,
            Some(git_context(
                "git@github.com:acme/widgets.git",
                "feature/demo",
            )),
        );
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_check = Arc::clone(&calls);
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            SandboxProvider::Docker,
            &prepared,
            &resolved,
            None,
            move |request, _github_app| {
                calls_for_check.lock().unwrap().push(request);
                async { Ok(()) }
            },
        )
        .await;

        assert!(ok);
        assert_eq!(calls.lock().unwrap().as_slice(), [GitRemoteRefCheck {
            origin_url: "https://github.com/acme/widgets".to_string(),
            branch:     Some("feature/demo".to_string()),
        }]);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "Repository Access");
        assert_eq!(checks[0].status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn repository_access_check_surfaces_remote_probe_failure() {
        let (prepared, resolved) = prepared_and_resolved_for_sandbox(
            SandboxProvider::Docker,
            false,
            Some(git_context("https://github.com/acme/widgets", "missing")),
        );
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            SandboxProvider::Docker,
            &prepared,
            &resolved,
            None,
            |_request, _github_app| async { Err("remote branch not found".to_string()) },
        )
        .await;

        assert!(!ok);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "Repository Access");
        assert_eq!(checks[0].status, CheckStatus::Error);
        assert!(
            checks[0]
                .remediation
                .as_deref()
                .unwrap_or_default()
                .contains("remote branch not found")
        );
    }

    #[test]
    fn preflight_sandbox_spec_disables_docker_clone_but_preserves_clone_metadata() {
        let (prepared, resolved) = prepared_and_resolved_for_sandbox(
            SandboxProvider::Docker,
            false,
            Some(git_context("https://github.com/acme/widgets", "main")),
        );

        let spec =
            preflight_sandbox_spec(SandboxProvider::Docker, &prepared, &resolved, None, None);

        match spec {
            SandboxSpec::Docker {
                config,
                clone_origin_url,
                clone_branch,
                ..
            } => {
                assert!(config.skip_clone);
                assert_eq!(
                    clone_origin_url.as_deref(),
                    Some("https://github.com/acme/widgets")
                );
                assert_eq!(clone_branch.as_deref(), Some("main"));
            }
            _ => panic!("expected Docker preflight sandbox spec"),
        }
    }

    #[test]
    fn workflow_bundle_rejects_duplicate_canonical_workflow_keys() {
        let workflows = HashMap::from([
            ("bar.fabro".to_string(), manifest_workflow()),
            ("./foo/../bar.fabro".to_string(), manifest_workflow()),
        ]);

        let error = workflow_bundle_from_manifest(&workflows).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("duplicate canonical workflow key: bar.fabro")
        );
    }

    #[test]
    fn workflow_bundle_rejects_duplicate_canonical_file_keys() {
        let mut workflow = manifest_workflow();
        workflow.files = HashMap::from([
            ("prompts/hello.md".to_string(), manifest_file("first")),
            ("./prompts/./hello.md".to_string(), manifest_file("second")),
        ]);
        let workflows = HashMap::from([("workflow.fabro".to_string(), workflow)]);

        let error = workflow_bundle_from_manifest(&workflows).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("duplicate canonical file key: prompts/hello.md")
        );
    }

    #[test]
    fn workflow_bundle_rejects_invalid_workflow_key() {
        let workflows = HashMap::from([("/abs/path.fabro".to_string(), manifest_workflow())]);

        let error = workflow_bundle_from_manifest(&workflows).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("invalid manifest workflow key: /abs/path.fabro")
        );
    }

    #[test]
    fn workflow_bundle_rejects_invalid_file_key() {
        let mut workflow = manifest_workflow();
        workflow.files = HashMap::from([("~/foo.md".to_string(), manifest_file("content"))]);
        let workflows = HashMap::from([("workflow.fabro".to_string(), workflow)]);

        let error = workflow_bundle_from_manifest(&workflows).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("invalid manifest file key: ~/foo.md")
        );
    }

    #[test]
    fn prepare_manifest_preserves_explicit_manifest_dry_run() {
        let server_settings = manifest_run_defaults(Some(&server_settings_fixture(
            r#"
_version = 1

[run.execution]
mode = "dry_run"

[server.storage]
root = "/srv/fabro"
"#,
        )));
        let mut manifest = minimal_manifest();
        manifest.args = Some(types::ManifestArgs {
            auto_approve:     None,
            dry_run:          Some(true),
            label:            Vec::new(),
            model:            None,
            no_retro:         None,
            preserve_sandbox: None,
            provider:         None,
            sandbox:          None,
            docker_image:     None,
            verbose:          None,
            worktree_mode:    None,
        });

        let prepared = prepare_manifest(&server_settings, &manifest).unwrap();

        assert_eq!(
            prepared.settings.run.execution.mode,
            fabro_types::settings::run::RunMode::DryRun
        );
    }

    #[test]
    fn prepare_manifest_prefers_bundled_settings_without_duplication() {
        let server_settings = manifest_run_defaults(Some(&server_settings_fixture(
            r#"
_version = 1

[server.storage]
root = "/srv/fabro"

[[run.prepare.steps]]
script = "cli-setup"

[server.integrations.github]
app_id = "fixture-app-id"
"#,
        )));

        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().config =
            Some(types::ManifestWorkflowConfig {
                path:   "workflow.toml".to_string(),
                source: r#"
_version = 1

[[run.prepare.steps]]
script = "workflow-setup"
"#
                .to_string(),
            });
        manifest.configs.push(types::ManifestConfig {
            path:   Some("/tmp/home/.fabro/settings.toml".to_string()),
            source: Some(
                r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[[run.prepare.steps]]
script = "cli-setup"

[server.integrations.github]
app_id = "fixture-app-id"
"#
                .to_string(),
            ),
            type_:  types::ManifestConfigType::User,
        });

        let prepared = prepare_manifest(&server_settings, &manifest).unwrap();
        let settings_json = serde_json::to_value(&prepared.settings).unwrap();

        // v2 merge matrix: run.prepare.steps replaces the whole list across
        // layers, so the higher-precedence workflow layer wins over cli.
        assert_eq!(prepared.settings.run.prepare.commands, vec![
            "workflow-setup".to_string()
        ]);
        assert!(settings_json.pointer("/server").is_none());
    }

    #[tokio::test]
    async fn invalid_preflight_returns_diagnostics_without_runtime_checks() {
        let state = crate::test_support::test_app_state();
        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &invalid_manifest(),
        )
        .unwrap();
        let validated = validate_prepared_manifest(&prepared).unwrap();

        assert!(validated.has_errors());

        let (response, ok) = run_preflight(state.as_ref(), &prepared, &validated)
            .await
            .unwrap();

        assert!(!ok);
        assert_eq!(response.workflow.name, "Invalid");
        assert!(!response.workflow.diagnostics.is_empty());
        assert_eq!(response.checks.title, "Run Preflight");
        assert_eq!(response.checks.sections.len(), 1);
        assert_eq!(response.checks.sections[0].checks.len(), 2);
    }

    #[tokio::test]
    async fn preflight_allows_pull_request_enabled_without_github_credentials() {
        let state = crate::test_support::test_app_state();
        let mut manifest = minimal_manifest();
        manifest.configs.push(types::ManifestConfig {
            path:   Some("/tmp/project/.fabro/project.toml".to_string()),
            source: Some(
                r#"
_version = 1

[run.pull_request]
enabled = true

[run.sandbox]
provider = "local"
"#
                .to_string(),
            ),
            type_:  types::ManifestConfigType::Project,
        });

        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_prepared_manifest(&prepared).unwrap();

        assert!(!validated.has_errors());

        let (response, ok) = run_preflight(state.as_ref(), &prepared, &validated)
            .await
            .unwrap();

        assert!(ok);
        assert!(response.workflow.diagnostics.is_empty());
        assert!(
            response.checks.sections[0]
                .checks
                .iter()
                .all(|check| check.name != "GitHub Token")
        );
    }

    #[tokio::test]
    async fn preflight_daytona_without_github_credentials_returns_report() {
        let state = crate::test_support::test_app_state();
        let mut manifest = minimal_manifest();
        manifest.configs.push(types::ManifestConfig {
            path:   Some("/tmp/project/.fabro/project.toml".to_string()),
            source: Some(
                r#"
_version = 1

[run.sandbox]
provider = "daytona"
"#
                .to_string(),
            ),
            type_:  types::ManifestConfigType::Project,
        });

        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_prepared_manifest(&prepared).unwrap();

        let (response, _ok) = run_preflight(state.as_ref(), &prepared, &validated)
            .await
            .unwrap();

        assert!(response.workflow.diagnostics.is_empty());
        assert!(
            response.checks.sections[0]
                .checks
                .iter()
                .any(|check| check.name == "Sandbox")
        );
    }

    #[tokio::test]
    async fn preflight_probes_configured_llm_model_availability() {
        let server = httpmock::MockServer::start_async().await;
        let response_mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/v1/responses")
                    .header("authorization", "Bearer test-openai-key");
                then.status(429)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({
                        "error": {
                            "message": "quota limited",
                            "type": "rate_limit_error"
                        }
                    }));
            })
            .await;
        let base_url = server.url("/v1");
        let state = crate::test_support::test_app_state_with_env_lookup(
            crate::test_support::default_test_server_settings(),
            RunLayer::default(),
            5,
            move |name| (name == "OPENAI_BASE_URL").then(|| base_url.clone()),
        );
        state
            .vault
            .write()
            .await
            .set(
                "openai",
                &serde_json::to_string(&fabro_auth::AuthCredential {
                    provider: Provider::OpenAi,
                    details:  fabro_auth::AuthDetails::ApiKey {
                        key: "test-openai-key".to_string(),
                    },
                })
                .unwrap(),
                fabro_vault::SecretType::Credential,
                None,
            )
            .unwrap();

        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().source = r#"
digraph Demo {
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    work  [prompt="Do work", model="gpt-54"]
    start -> work -> exit
}
"#
        .to_string();
        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_prepared_manifest(&prepared).unwrap();

        let (response, ok) = run_preflight(state.as_ref(), &prepared, &validated)
            .await
            .unwrap();

        assert!(!ok);
        let llm_check = response.checks.sections[0]
            .checks
            .iter()
            .find(|check| check.name == "LLM" && check.summary == "gpt-5.4")
            .expect("preflight should include the configured LLM model");
        assert_eq!(llm_check.status, types::PreflightCheckResultStatus::Error);
        assert!(
            llm_check
                .remediation
                .as_deref()
                .unwrap_or_default()
                .contains("Rate limited by openai: quota limited")
        );
        assert!(response_mock.calls_async().await >= 1);
    }
}
