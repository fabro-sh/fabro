use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use fabro_api::types;
use fabro_config::parse::SettingsSource;
use fabro_config::{
    CliLayer, CliOutputLayer, EnvironmentLayer, MergeMap, RunLayer, SettingsLayer,
    WorkflowSettingsBuilder, parse_input_overrides, parse_labels, project,
};
use fabro_github::token_source::{InstallationTokenSource, ResolvedToken, TokenSnapshot};
use fabro_graphviz::graph::{Graph, is_llm_handler_type};
use fabro_graphviz::render::apply_direction;
use fabro_llm::lithos_catalog::Catalog;
use fabro_llm::probe::{self, ModelTestStatus};
use fabro_llm::{FabroClient, selection};
use fabro_petri::check::Launch;
use fabro_petri::runtime::RuntimeSpec;
use fabro_proc::ProcessError;
use fabro_sandbox::{
    CloneRequest, ProviderAccess, RunSandbox, SandboxSpec, sandbox_spec_for_environment,
};
use fabro_static::EnvVars;
use fabro_types::diagnostic::{Diagnostic, Severity};
use fabro_types::settings::cli::OutputVerbosity;
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::run::{McpServerSettings, RunGoal, RunNamespace};
use fabro_types::{
    BundledProvider, ManifestPath, RunId, SandboxProviderKind, ServerSettings, WorkflowSettings,
};
use fabro_util::check_report::{CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus};
use fabro_workflow::Error as WorkflowError;
use fabro_workflow::operations::{ValidateInput, WorkflowInput, validate};
use fabro_workflow::pipeline::Validated;
use fabro_workflow::workflow_bundle::{BundledWorkflow, ParsedWorkflowConfig, WorkflowBundle};
use futures_util::stream::{self, StreamExt};
use lithos_llm::catalog::ProviderId;
use tokio::process::Command;
#[cfg(test)]
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::server::AppState;
use crate::{petri_check, run_compiler};

#[derive(Clone)]
pub(crate) struct PreparedManifest {
    pub cwd:              PathBuf,
    pub git:              Option<types::GitContext>,
    pub root_source:      String,
    pub settings:         WorkflowSettings,
    pub target_path:      ManifestPath,
    pub workflow_bundle:  WorkflowBundle,
    pub workflow_input:   BundledWorkflow,
    pub source_directory: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ManifestSettingsOverrides {
    pub(crate) run:             Option<RunLayer>,
    pub(crate) cli:             Option<CliLayer>,
    pub(crate) input_overrides: HashMap<String, toml::Value>,
}

#[cfg(test)]
pub(crate) fn manifest_run_defaults(run: Option<&RunLayer>) -> RunLayer {
    run.cloned().unwrap_or_default()
}

pub(crate) fn prepare_manifest_with_environment_defaults(
    manifest_run_defaults: &RunLayer,
    manifest_environment_defaults: &MergeMap<EnvironmentLayer>,
    manifest_mcp_server_catalog: &HashMap<String, McpServerSettings>,
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

    let args_overrides =
        manifest_args_overrides(manifest.args.as_ref()).context("failed to parse manifest args")?;
    let mut workflow_settings_builder = WorkflowSettingsBuilder::new()
        .server_manifest_defaults(
            manifest_run_defaults.clone(),
            manifest_environment_defaults.clone(),
        )
        .server_mcp_catalog(manifest_mcp_server_catalog.clone());
    if let Some(run) = args_overrides.run {
        workflow_settings_builder = workflow_settings_builder.run_overrides(run);
    }
    if let Some(cli) = args_overrides.cli {
        workflow_settings_builder = workflow_settings_builder.cli_overrides(cli);
    }
    if let Some(config) = workflow_input.config.as_ref() {
        let layer = settings_layer_with_resolved_dockerfiles(
            &config.source,
            &config.path,
            &workflow_input.files,
            SettingsSource::Workflow,
        )?;
        workflow_settings_builder = workflow_settings_builder.workflow_layer(layer);
    }
    for config in manifest
        .configs
        .iter()
        .filter(|config| config.type_ == types::ManifestConfigType::Project)
    {
        if let Some(source) = config.source.as_deref() {
            let config_path = manifest_project_config_path(config, &cwd)?;
            let layer = settings_layer_with_resolved_dockerfiles(
                source,
                &config_path,
                &workflow_input.files,
                SettingsSource::Project,
            )?;
            workflow_settings_builder = workflow_settings_builder.project_layer(layer);
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
    settings.run.inputs.extend(args_overrides.input_overrides);
    if let Some(goal) = manifest
        .goal
        .as_ref()
        .filter(|goal| goal.type_ != types::ManifestGoalType::Graph)
    {
        settings.run.goal = Some(RunGoal::Inline(InterpString::parse(&goal.text)));
    }
    manifest
        .title
        .as_ref()
        .map(|title| fabro_types::normalize_explicit_run_title(title.as_str()))
        .transpose()?;
    manifest
        .parent_id
        .as_deref()
        .map(str::parse::<RunId>)
        .transpose()
        .context("invalid parent run ID")?;

    let source_directory = project::resolve_working_directory_from_run(&settings.run, &cwd);
    Ok(PreparedManifest {
        cwd,
        git: manifest.git.clone(),
        root_source,
        settings,
        target_path,
        workflow_bundle,
        workflow_input,
        source_directory,
    })
}

/// Fabro's own structural pass: parse and transform the root workflow, with
/// the transforms' diagnostics. Enough to render a graph.
pub(crate) fn validate_prepared_manifest_structural(
    prepared: &PreparedManifest,
) -> Result<Validated, WorkflowError> {
    validate(manifest_validate_input(prepared, HashMap::new()))
}

/// The structural pass, then Petri's check of the whole bundle under
/// `launch` and `runtime`, its diagnostics after the transforms' own.
/// `has_ready_provider` false adds Fabro's refusal of a model node no
/// provider can run; `unbound_is_warning` keeps an input nothing binds a
/// warning, for a validation before the run's inputs exist. Blocking:
/// Petri lowers the graph synchronously.
pub(crate) fn validate_prepared_manifest(
    prepared: &PreparedManifest,
    vars: &HashMap<String, String>,
    launch: Launch,
    runtime: RuntimeSpec,
    has_ready_provider: bool,
    unbound_is_warning: bool,
) -> Result<Validated, WorkflowError> {
    let mut validated = validate(manifest_validate_input(prepared, vars.clone()))?;
    let request = petri_check::check_request(
        &prepared.workflow_bundle,
        &prepared.target_path,
        &prepared.settings,
        vars,
        launch,
        runtime,
        unbound_is_warning,
    )?;
    let checked = petri_check::check(&request, has_ready_provider)?;
    validated.extend_diagnostics(checked.diagnostics);
    Ok(validated)
}

/// The model and provider the run's LLM nodes default to: what the settings
/// or the graph name, resolved against the ready providers first and the
/// whole catalog after, as the run's launch binds them.
fn preflight_model(
    catalog: &Catalog,
    graph: &Graph,
    settings: &WorkflowSettings,
    ready_providers: &[ProviderId],
) -> Result<(String, ProviderId)> {
    let graph_attr = |name: &str| {
        graph
            .attrs
            .get(name)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    };
    let model = settings
        .run
        .model
        .name
        .clone()
        .or_else(|| graph_attr("default_model"));
    let provider = settings
        .run
        .model
        .provider
        .clone()
        .or_else(|| graph_attr("default_provider"))
        .filter(|provider| !provider.is_empty())
        .map(ProviderId::new);
    let eligible = ready_providers.iter().cloned().collect();
    let selected = selection::resolve_selection_with_catalog_fallback(
        catalog,
        model.as_deref(),
        provider.as_ref(),
        &eligible,
    )?;
    Ok((selected.model, selected.provider))
}

fn manifest_validate_input(
    prepared: &PreparedManifest,
    vars: HashMap<String, String>,
) -> ValidateInput {
    ValidateInput {
        workflow: WorkflowInput::Bundled(prepared.workflow_input.clone()),
        settings: prepared.settings.clone(),
        vars,
        cwd: prepared.cwd.clone(),
        custom_transforms: Vec::new(),
    }
}

pub(crate) async fn run_preflight(
    state: &AppState,
    prepared: &PreparedManifest,
    validated: &Validated,
    llm_result: Result<FabroClient>,
) -> Result<(types::PreflightResponse, bool)> {
    let (report, checks_ok) =
        build_preflight_report(state, prepared, validated, llm_result).await?;
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

fn settings_layer_with_resolved_dockerfiles(
    source: &str,
    config_path: &ManifestPath,
    files: &HashMap<ManifestPath, String>,
    settings_source: SettingsSource,
) -> Result<SettingsLayer> {
    run_compiler::settings_layer_with_resolved_dockerfiles(
        source,
        config_path,
        files,
        settings_source,
    )
    .map_err(anyhow::Error::new)
}

pub(crate) fn manifest_args_overrides(
    args: Option<&types::ManifestArgs>,
) -> Result<ManifestSettingsOverrides> {
    let Some(args) = args else {
        return Ok(ManifestSettingsOverrides::default());
    };

    let run = fabro_manifest::build_sparse_run_overrides(fabro_manifest::RunOverrideInput {
        goal:             None,
        model:            args.model.as_deref(),
        provider:         args.provider.as_deref(),
        environment:      args.environment.as_deref(),
        preserve_sandbox: args.preserve_sandbox,
        dry_run:          args.dry_run,
        auto_approve:     args.auto_approve,
        labels:           parse_labels(&args.label),
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

    Ok(ManifestSettingsOverrides {
        run,
        cli,
        input_overrides: parse_input_overrides(&args.input)?,
    })
}

fn manifest_project_config_path(
    config: &types::ManifestConfig,
    cwd: &Path,
) -> Result<ManifestPath> {
    let path = config
        .path
        .as_deref()
        .ok_or_else(|| anyhow!("invalid manifest project config path: missing path"))?;
    let path_ref = Path::new(path);
    let manifest_path = if path_ref.is_absolute() {
        ManifestPath::from_absolute(path_ref, cwd)
    } else {
        ManifestPath::from_wire(path)
    };
    manifest_path.ok_or_else(|| anyhow!("invalid manifest project config path: {path}"))
}

async fn build_preflight_report(
    state: &AppState,
    prepared: &PreparedManifest,
    validated: &Validated,
    llm_result: Result<FabroClient>,
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

    let catalog = state.catalog();
    let ready_providers = llm_result
        .as_ref()
        .map(FabroClient::provider_ids)
        .unwrap_or_default();
    let (run_model, run_provider) = preflight_model(
        catalog.as_ref(),
        graph,
        &prepared.settings,
        &ready_providers,
    )?;
    let run_provider = run_provider.into_string();
    let (run_model, run_provider) = (run_model.as_str(), run_provider.as_str());
    let resolved_run = prepared.settings.run.clone();
    let server_settings = state.server_settings();
    let github_integration = &server_settings.server.integrations.github;
    let sandbox_provider = effective_sandbox_provider(&resolved_run);
    if let Some(error) = sandbox_provider_policy_error(&server_settings, &sandbox_provider) {
        checks.push(CheckResult {
            name:        "Sandbox Provider Policy".into(),
            status:      CheckStatus::Error,
            summary:     error,
            details:     Vec::new(),
            remediation: None,
        });
        return Ok((
            CheckReport {
                title:    "Run Preflight".into(),
                sections: vec![CheckSection {
                    title: String::new(),
                    checks,
                }],
            },
            false,
        ));
    }
    run_environment_capability_check(&mut checks, &resolved_run);
    let needs_github_credentials = sandbox_provider.clones_workspace()
        || resolved_run.integrations.github.is_token_requested();
    let github_app = if needs_github_credentials {
        match state.github_credentials(github_integration).await {
            Ok(credentials) => credentials,
            Err(err)
                if err
                    .downcast_ref::<fabro_vault::SecretStoreError>()
                    .is_some() =>
            {
                return Err(err);
            }
            Err(_) => None,
        }
    } else {
        None
    };

    let access = state.provider_access().await?;
    let sandbox_ok = run_sandbox_check(
        &mut checks,
        &sandbox_provider,
        prepared,
        &resolved_run,
        github_app.clone(),
        &access,
    )
    .await;
    let repository_access_ok = run_repository_access_check(
        &mut checks,
        &sandbox_provider,
        prepared,
        &resolved_run,
        github_app.clone(),
    )
    .await;
    let llm_ok = run_llm_check(
        &mut checks,
        graph,
        run_model,
        run_provider,
        catalog.as_ref(),
        llm_result,
    )
    .await;
    let github_token_ok =
        run_github_token_check(&mut checks, prepared, &resolved_run, github_app).await;

    let checks_ok = sandbox_ok && repository_access_ok && llm_ok && github_token_ok;

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
    let setup_command_count = prepared.settings.run.prepare.steps.len();
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

pub(crate) fn sandbox_provider_policy_error(
    server_settings: &ServerSettings,
    provider: &SandboxProviderKind,
) -> Option<String> {
    let providers = &server_settings.server.sandbox.providers;
    match providers.get(provider) {
        Some(entry) if entry.enabled => None,
        Some(_) => Some(format!(
            "sandbox provider \"{provider}\" is disabled by server.sandbox.providers.{provider}.enabled"
        )),
        None => Some(format!(
            "sandbox provider \"{provider}\" is not configured; add [server.sandbox.providers.{provider}] to settings.toml"
        )),
    }
}

pub(crate) fn configured_sandbox_provider(settings: &RunNamespace) -> SandboxProviderKind {
    settings.environment.provider.clone()
}

pub(crate) fn effective_sandbox_provider(settings: &RunNamespace) -> SandboxProviderKind {
    configured_sandbox_provider(settings).effective_for(settings.execution.mode)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitRemoteRefCheck {
    origin_url: String,
    branch:     Option<String>,
}

fn clone_disabled_for_provider(
    provider: &SandboxProviderKind,
    resolved_run: &RunNamespace,
) -> bool {
    provider.clones_workspace() && !resolved_run.clone.enabled
}

fn run_environment_capability_check(checks: &mut Vec<CheckResult>, resolved_run: &RunNamespace) {
    let warnings = environment_capability_warnings(resolved_run);
    if warnings.is_empty() {
        return;
    }
    checks.push(CheckResult {
        name:        "Environment Capabilities".into(),
        status:      CheckStatus::Warning,
        summary:     format!("{} unsupported hint(s) ignored", warnings.len()),
        details:     warnings
            .into_iter()
            .map(|text| CheckDetail { text, warn: true })
            .collect(),
        remediation: None,
    });
}

fn environment_capability_warnings(resolved_run: &RunNamespace) -> Vec<String> {
    let environment = &resolved_run.environment;
    let mut warnings = Vec::new();
    match environment.provider.bundled() {
        Some(BundledProvider::Local) => {
            if environment.resources.cpu.is_some()
                || environment.resources.memory.is_some()
                || environment.resources.disk.is_some()
            {
                warnings.push("local provider ignores resource limits".to_string());
            }
            if !environment.labels.is_empty() {
                warnings.push("local provider ignores labels".to_string());
            }
            if environment.lifecycle.auto_stop.is_some() {
                warnings.push("local provider ignores lifecycle.auto_stop".to_string());
            }
        }
        Some(BundledProvider::Docker) => {
            if environment.cwd.is_some() {
                warnings.push("docker provider ignores cwd".to_string());
            }
            if environment.resources.disk.is_some() {
                warnings.push("docker provider ignores disk resource limits".to_string());
            }
            if !environment.labels.is_empty() {
                warnings.push("docker provider ignores labels".to_string());
            }
            if environment.lifecycle.auto_stop.is_some() {
                warnings.push("docker provider ignores lifecycle.auto_stop".to_string());
            }
            if environment.image.dockerfile.is_some() {
                warnings.push("docker provider ignores image.dockerfile".to_string());
            }
        }
        Some(BundledProvider::Daytona) => {
            if environment.cwd.is_some() {
                warnings.push("daytona provider ignores cwd".to_string());
            }
        }
        None => {
            if environment.cwd.is_some() {
                warnings.push(format!("{} provider ignores cwd", environment.provider));
            }
        }
    }
    warnings
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
    sandbox_provider: &SandboxProviderKind,
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
    sandbox_provider: &SandboxProviderKind,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
    check_remote_ref: F,
) -> bool
where
    F: FnOnce(GitRemoteRefCheck, Option<fabro_github::GitHubCredentials>) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    if !sandbox_provider.clones_workspace()
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

    run_ls_remote(command)
        .await
        .map_err(|message| match &auth_url {
            Some(auth_url) => auth_url.redact_in(&message),
            None => message,
        })
}

/// Run a prepared `git ls-remote` invocation with a 10s timeout, reducing a
/// failure to its most useful message: stderr, then stdout, then the exit
/// status.
async fn run_ls_remote(mut command: Command) -> std::result::Result<(), String> {
    let output = fabro_proc::capture(
        &mut command,
        Some(Duration::from_secs(10)),
        &CancellationToken::new(),
        None,
    )
    .await
    .map_err(|err| match err {
        ProcessError::TimedOut => "git ls-remote timed out after 10s".to_string(),
        ProcessError::Io(source) => format!("Failed to run git ls-remote: {source}"),
        ProcessError::Cancelled => "git ls-remote cancelled".to_string(),
    })?
    .output;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Err(if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("git ls-remote exited with status {}", output.status)
    })
}

fn preflight_sandbox_spec(
    sandbox_provider: &SandboxProviderKind,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
    access: &ProviderAccess,
) -> std::result::Result<SandboxSpec, fabro_sandbox::Error> {
    let clone_origin_url = prepared
        .git
        .as_ref()
        .map(|git| fabro_github::normalize_repo_origin_url(&git.origin_url));
    let clone_branch = prepared.git.as_ref().map(|git| git.branch.clone());

    if sandbox_provider.bundled() == Some(BundledProvider::Local) {
        let working_directory = resolved_run
            .environment
            .local_working_directory(Some(&prepared.source_directory))
            .map_err(|err| {
                fabro_sandbox::Error::context(
                    "Failed to resolve local environment working directory",
                    err,
                )
            })?;
        return Ok(SandboxSpec::local(working_directory, access.clone()));
    }
    // No vault is available on this path, so a `{{ secrets.* }}` value keeps
    // its source form. Preflight never clones.
    let spec = sandbox_spec_for_environment(
        &resolved_run.environment,
        resolved_run.environment.unresolved_env(),
    )?;
    let clone = CloneRequest {
        origin_url: clone_origin_url,
        branch: clone_branch,
        ..CloneRequest::none()
    };
    Ok(SandboxSpec {
        kind: sandbox_provider.clone(),
        access: access.clone(),
        spec,
        clone,
        github_app,
        run_id: None,
    })
}

async fn run_sandbox_check(
    checks: &mut Vec<CheckResult>,
    sandbox_provider: &SandboxProviderKind,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
    access: &ProviderAccess,
) -> bool {
    let spec = match preflight_sandbox_spec(
        sandbox_provider,
        prepared,
        resolved_run,
        github_app.clone(),
        access,
    ) {
        Ok(spec) => spec,
        Err(err) => {
            checks.push(CheckResult {
                name:        "Sandbox".into(),
                status:      CheckStatus::Error,
                summary:     "failed".into(),
                details:     vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                remediation: Some(err.to_string()),
            });
            return false;
        }
    };
    let sandbox_result: Result<Arc<RunSandbox>, String> = spec.build(None).await.map_err(|err| {
        if *sandbox_provider == SandboxProviderKind::DAYTONA {
            format!("Daytona sandbox creation failed: {err}")
        } else {
            err.to_string()
        }
    });

    match sandbox_result {
        Ok(sandbox) => match sandbox.initialize().await {
            Ok(()) => {
                let mut details = vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))];
                if sandbox_provider.clones_workspace()
                    && prepared.git.is_none()
                    && !clone_disabled_for_provider(sandbox_provider, resolved_run)
                {
                    details.push(CheckDetail {
                        text: "No clone source present; sandbox workspace will be empty".into(),
                        warn: true,
                    });
                }
                if let Err(err) = sandbox.delete().await {
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
                let cleanup_error = sandbox.delete().await.err();
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

const MODEL_PREFLIGHT_PROBE_CONCURRENCY: usize = 4;

struct PendingModelProbe {
    index:         usize,
    model_id:      String,
    provider_name: String,
}

async fn run_llm_check(
    checks: &mut Vec<CheckResult>,
    graph: &Graph,
    model: &str,
    default_provider: &str,
    catalog: &Catalog,
    llm_result: Result<FabroClient>,
) -> bool {
    let mut model_providers = std::collections::BTreeSet::new();
    let mut has_llm_nodes = false;
    // Each node's selector resolves against the ready providers first and
    // the whole catalog after, as the run's launch binds it: an alias
    // becomes the catalog model on the provider that offers it. A selector
    // the catalog cannot place stays as written; the checks below say why.
    let eligible = llm_result
        .as_ref()
        .map(FabroClient::provider_ids)
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();

    for node in graph.nodes.values() {
        if !is_llm_handler_type(node.handler_type()) {
            continue;
        }
        has_llm_nodes = true;
        let node_model = node.model().unwrap_or(model);
        let node_provider = node.provider().unwrap_or(default_provider);
        // Only a provider the node names itself constrains the selection:
        // an unqualified alias goes to whichever eligible provider offers it.
        let provider = node
            .provider()
            .filter(|provider| !provider.is_empty())
            .map(ProviderId::new);
        let resolved = selection::resolve_selection_with_catalog_fallback(
            catalog,
            Some(node_model),
            provider.as_ref(),
            &eligible,
        )
        .map_or_else(
            |_| (node_model.to_string(), node_provider.to_string()),
            |selected| (selected.model, selected.provider.into_string()),
        );
        model_providers.insert(resolved);
    }

    if !has_llm_nodes {
        return true;
    }

    match llm_result {
        Ok(result) => {
            let auth_issues = result.auth_issues;
            let registration_issues = result.build_issues;
            let client = Arc::new(result.client);

            let mut all_ok = true;
            let mut completed_checks: Vec<(usize, CheckResult)> = Vec::new();
            let mut pending_probes = Vec::new();
            for (index, (model_id, provider_name)) in model_providers.iter().enumerate() {
                let provider_id = canonical_provider_id(catalog, provider_name);
                if let Some((_, issue)) = auth_issues
                    .iter()
                    .find(|(candidate, _)| candidate == &provider_id)
                {
                    all_ok = false;
                    completed_checks.push((index, CheckResult {
                        name:        "LLM".into(),
                        status:      CheckStatus::Warning,
                        summary:     model_id.clone(),
                        details:     vec![CheckDetail::new(format!("Provider: {provider_name}"))],
                        remediation: Some(issue.to_string()),
                    }));
                } else if let Some(issue) = registration_issues
                    .iter()
                    .find(|issue| issue.provider == provider_id)
                {
                    all_ok = false;
                    completed_checks.push((index, CheckResult {
                        name:        "LLM".into(),
                        status:      CheckStatus::Warning,
                        summary:     model_id.clone(),
                        details:     vec![CheckDetail::new(format!("Provider: {provider_name}"))],
                        remediation: Some(issue.cause.to_string()),
                    }));
                } else if !client.available_providers().contains(&provider_id) {
                    all_ok = false;
                    completed_checks.push((index, CheckResult {
                        name:        "LLM".into(),
                        status:      CheckStatus::Warning,
                        summary:     model_id.clone(),
                        details:     vec![CheckDetail::new(format!("Provider: {provider_name}"))],
                        remediation: Some(format!(
                            "Provider \"{provider_name}\" is not configured"
                        )),
                    }));
                } else {
                    pending_probes.push(PendingModelProbe {
                        index,
                        model_id: model_id.clone(),
                        provider_name: provider_name.clone(),
                    });
                }
            }

            let mut probe_checks = stream::iter(pending_probes)
                .map(|probe| {
                    let client = Arc::clone(&client);
                    async move {
                        let outcome = probe::run_basic_probe(
                            &client,
                            &format!("{}/{}", probe.provider_name, probe.model_id),
                            Duration::from_secs(fabro_types::ModelTestMode::Basic.timeout_secs()),
                        )
                        .await;
                        let (status, remediation) = if outcome.status == ModelTestStatus::Ok {
                            (CheckStatus::Pass, None)
                        } else {
                            (
                                CheckStatus::Error,
                                Some(format!(
                                    "Model availability probe failed: {}",
                                    outcome
                                        .error_message
                                        .unwrap_or_else(|| "unknown error".to_string())
                                )),
                            )
                        };
                        (probe.index, CheckResult {
                            name: "LLM".into(),
                            status,
                            summary: probe.model_id,
                            details: vec![
                                CheckDetail::new(format!("Provider: {}", probe.provider_name)),
                                CheckDetail::new("Probe: basic generation".to_string()),
                            ],
                            remediation,
                        })
                    }
                })
                .buffer_unordered(MODEL_PREFLIGHT_PROBE_CONCURRENCY)
                .collect::<Vec<_>>()
                .await;

            if probe_checks
                .iter()
                .any(|(_, check)| check.status != CheckStatus::Pass)
            {
                all_ok = false;
            }
            completed_checks.append(&mut probe_checks);
            completed_checks.sort_by_key(|(index, _)| *index);
            checks.extend(completed_checks.into_iter().map(|(_, check)| check));
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

fn canonical_provider_id(catalog: &Catalog, provider_name: &str) -> ProviderId {
    catalog.enabled_provider(provider_name).map_or_else(
        || ProviderId::new(provider_name),
        |provider| provider.id().clone(),
    )
}

async fn run_github_token_check(
    checks: &mut Vec<CheckResult>,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
) -> bool {
    run_github_token_check_with(
        checks,
        prepared,
        resolved_run,
        github_app,
        mint_scoped_github_token,
        probe_github_repository,
    )
    .await
}

async fn run_github_token_check_with<M, MFut, P, PFut>(
    checks: &mut Vec<CheckResult>,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
    mint_scoped_token: M,
    probe_repository: P,
) -> bool
where
    M: FnOnce(fabro_github::GitHubRepositoryAccess, fabro_github::GitHubCredentials) -> MFut,
    MFut: Future<Output = std::result::Result<ResolvedToken, String>>,
    P: Fn(fabro_types::GitHubRepositorySlug, ResolvedToken) -> PFut,
    PFut: Future<Output = std::result::Result<(), String>>,
{
    let github = &resolved_run.integrations.github;
    if !github.is_token_requested() {
        return true;
    }

    // Resolve InterpString permission values eagerly for token minting and
    // for display in the preflight report.
    let integration = match github.resolve_integration() {
        Ok(integration) => integration,
        Err(err) => {
            return fail_github_token_check(
                checks,
                Vec::new(),
                "invalid permissions",
                format!("Failed to resolve GitHub permissions: {err}"),
            );
        }
    };
    let perm_details = integration
        .permissions
        .iter()
        .map(|(key, value)| CheckDetail::new(format!("{key}: {value}")))
        .collect::<Vec<_>>();

    if !integration.has_additional_repositories() {
        // Primary-only behavior is unchanged: a mint check when credentials
        // and an origin exist, a warning otherwise, and no Git-content probe
        // (permissions-only workflows may request non-contents permissions).
        return check_primary_only_github_token(
            checks,
            prepared,
            github_app,
            &integration.permissions,
            perm_details,
        )
        .await;
    }

    // `gh` checks GH_TOKEN before GITHUB_TOKEN, so a user-defined GH_TOKEN
    // bypasses the managed scoped token for gh commands. Warn without
    // failing; the value is the workflow author's responsibility.
    if resolved_run.environment.env.contains_key(EnvVars::GH_TOKEN) {
        checks.push(CheckResult {
            name:        "GH_TOKEN Override".into(),
            status:      CheckStatus::Warning,
            summary:     "gh will not use the managed token".into(),
            details:     vec![],
            remediation: Some(
                "The resolved run environment defines GH_TOKEN, which the gh CLI prefers over \
                 the managed GITHUB_TOKEN; gh commands will not use the token scoped to the \
                 declared repositories."
                    .to_string(),
            ),
        });
    }

    let Some(origin_url) = prepared
        .git
        .as_ref()
        .map(|git| git.origin_url.trim())
        .filter(|url| !url.is_empty())
    else {
        return fail_github_token_check(
            checks,
            perm_details,
            "missing origin",
            "run.integrations.github.additional_repositories requires a GitHub run origin, but \
             this run has no repository origin URL"
                .to_string(),
        );
    };
    let Some(creds) = github_app else {
        return fail_github_token_check(
            checks,
            perm_details,
            "missing credentials",
            "run.integrations.github.additional_repositories requires GitHub credentials, but \
             none are configured on the server"
                .to_string(),
        );
    };
    // The same validated access value runtime initialization constructs, so
    // preflight and runtime cannot disagree about the effective set.
    let access = match fabro_github::GitHubRepositoryAccess::new(
        Some(origin_url),
        &integration.additional_repositories,
        integration.permissions.clone(),
    ) {
        Ok(Some(access)) => access,
        // `new` returns `Ok(None)` only when nothing is declared, and the
        // declared set is non-empty here. Fail closed instead of panicking.
        Ok(None) => {
            return fail_github_token_check(
                checks,
                perm_details,
                "missing origin",
                "run.integrations.github.additional_repositories requires a GitHub run origin, \
                 but this run has no repository origin URL"
                    .to_string(),
            );
        }
        Err(err) => {
            return fail_github_token_check(
                checks,
                perm_details,
                "invalid repository set",
                format!("{err:#}"),
            );
        }
    };

    // One mint scoped to the whole effective set. In App mode the minter
    // first resolves every repository's installation so a failure names the
    // repository the App cannot see.
    let token = match mint_scoped_token(access.clone(), creds).await {
        Ok(token) => token,
        Err(err) => {
            return fail_github_token_check(checks, perm_details, "failed", err);
        }
    };
    checks.push(CheckResult {
        name:        "GitHub Token".into(),
        status:      CheckStatus::Pass,
        summary:     "minted".into(),
        details:     perm_details.clone(),
        remediation: None,
    });

    // Probe every effective repository with bounded concurrency, then report
    // in deterministic primary-first order. Possession of a scoped token is
    // not proof of access; the probe also verifies PAT/static credentials.
    let targets: Vec<fabro_types::GitHubRepositorySlug> =
        access.targets().into_iter().cloned().collect();
    let probe_repository = &probe_repository;
    let mut results: Vec<(usize, CheckResult)> =
        stream::iter(targets.into_iter().enumerate().map(|(index, slug)| {
            let token = token.clone();
            let perm_details = perm_details.clone();
            async move {
                let check = match probe_repository(slug.clone(), token).await {
                    Ok(()) => CheckResult {
                        name:        format!("GitHub Repository ({slug})"),
                        status:      CheckStatus::Pass,
                        summary:     "reachable".into(),
                        details:     perm_details,
                        remediation: None,
                    },
                    Err(err) => CheckResult {
                        name:        format!("GitHub Repository ({slug})"),
                        status:      CheckStatus::Error,
                        summary:     "failed".into(),
                        details:     perm_details,
                        remediation: Some(format!("Failed to verify repository access: {err}")),
                    },
                };
                (index, check)
            }
        }))
        .buffer_unordered(REPOSITORY_PROBE_CONCURRENCY)
        .collect()
        .await;
    results.sort_by_key(|(index, _)| *index);

    let mut ok = true;
    for (_, check) in results {
        if check.status != CheckStatus::Pass {
            ok = false;
        }
        checks.push(check);
    }
    ok
}

/// Report one "GitHub Token" preflight failure and fail the check.
fn fail_github_token_check(
    checks: &mut Vec<CheckResult>,
    perm_details: Vec<CheckDetail>,
    summary: &str,
    remediation: String,
) -> bool {
    checks.push(CheckResult {
        name:        "GitHub Token".into(),
        status:      CheckStatus::Error,
        summary:     summary.into(),
        details:     perm_details,
        remediation: Some(remediation),
    });
    false
}

/// Bounded concurrency for per-repository `git ls-remote` probes.
const REPOSITORY_PROBE_CONCURRENCY: usize = 4;

async fn check_primary_only_github_token(
    checks: &mut Vec<CheckResult>,
    prepared: &PreparedManifest,
    github_app: Option<fabro_github::GitHubCredentials>,
    permissions: &HashMap<String, String>,
    perm_details: Vec<CheckDetail>,
) -> bool {
    if let (Some(creds), Some(git)) = (&github_app, prepared.git.as_ref()) {
        match mint_github_token(creds, &git.origin_url, permissions).await {
            Ok(_) => {
                checks.push(CheckResult {
                    name:        "GitHub Token".into(),
                    status:      CheckStatus::Pass,
                    summary:     "minted".into(),
                    details:     perm_details,
                    remediation: None,
                });
                true
            }
            Err(err) => {
                checks.push(CheckResult {
                    name:        "GitHub Token".into(),
                    status:      CheckStatus::Error,
                    summary:     "failed".into(),
                    details:     perm_details,
                    remediation: Some(format!("Failed to mint GitHub token: {err}")),
                });
                false
            }
        }
    } else {
        checks.push(CheckResult {
            name:        "GitHub Token".into(),
            status:      CheckStatus::Warning,
            summary:     "skipped".into(),
            details:     perm_details,
            remediation: Some("No GitHub credentials or origin URL available".to_string()),
        });
        true
    }
}

/// Production minter for the multi-repository path. The source owns the
/// effective set. In App mode its first resolve checks every repository's
/// installation before minting the scoped token.
async fn mint_scoped_github_token(
    access: fabro_github::GitHubRepositoryAccess,
    creds: fabro_github::GitHubCredentials,
) -> std::result::Result<ResolvedToken, String> {
    let source =
        InstallationTokenSource::for_access(&creds, &access).map_err(|err| format!("{err:#}"))?;
    source.resolve().await.map_err(|err| format!("{err:#}"))
}

/// Production per-repository probe: a non-interactive
/// `git ls-remote <https-url> HEAD` authenticated through
/// [`fabro_github::GITHUB_CREDENTIAL_HELPER`] reading `GITHUB_TOKEN` from
/// the child process environment, so the token never appears in the URL,
/// argv, or rendered errors — exactly what the runtime `git_bridge`
/// configures in `fabro-workflow`.
async fn probe_github_repository(
    slug: fabro_types::GitHubRepositorySlug,
    token: ResolvedToken,
) -> std::result::Result<(), String> {
    let url = slug.https_url();
    probe_with_replication_retry(token.snapshot, || run_probe_ls_remote(&url, &token)).await
}

/// Retry auth-shaped failures with the SAME token: replication of a given
/// token only makes progress, while re-minting would restart the replication
/// clock. The driver's git retry owns the decision and the pacing; fabro's
/// probe policy owns the attempt count.
async fn probe_with_replication_retry<F, Fut>(
    snapshot: TokenSnapshot,
    run: F,
) -> std::result::Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<(), String>>,
{
    fabro_sandbox::retry_git_messages(
        &fabro_sandbox::repository_probe_policy(),
        Some(&snapshot),
        "repository probe",
        run,
    )
    .await
}

async fn run_probe_ls_remote(url: &str, token: &ResolvedToken) -> std::result::Result<(), String> {
    let mut command = Command::new("git");
    fabro_github::apply_probe_git_env(&mut command, token.token.expose());
    command.args(["ls-remote", url, "HEAD"]);
    run_ls_remote(command).await
}

async fn mint_github_token(
    creds: &fabro_github::GitHubCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> Result<String> {
    let https_url = fabro_github::ssh_url_to_https(origin_url);
    let (owner, repo) = fabro_github::parse_github_owner_repo(&https_url)?;
    let client = fabro_http::http_client()?;
    let perms_json = serde_json::to_value(permissions)?;
    creds
        .resolve_bearer_token(
            &client,
            &owner,
            &repo,
            &fabro_github::github_api_base_url(),
            perms_json,
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

pub(crate) fn workflow_summary(
    validated: &Validated,
    target_path: &Path,
) -> types::PreflightWorkflowSummary {
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

fn diagnostics_to_api(diagnostics: &[Diagnostic]) -> Vec<types::WorkflowDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| types::WorkflowDiagnostic {
            column:      diagnostic
                .column
                .and_then(|value| i32::try_from(value).ok()),
            edge:        diagnostic
                .edge
                .as_ref()
                .map(|edge: &(String, String)| [edge.0.clone(), edge.1.clone()]),
            fix:         diagnostic.fix.clone(),
            line:        diagnostic.line.and_then(|value| i32::try_from(value).ok()),
            message:     diagnostic.message.clone(),
            node_id:     diagnostic.node_id.clone(),
            related:     diagnostic
                .related
                .iter()
                .map(|related| types::RelatedWorkflowDiagnostic {
                    column:      related.column.and_then(|value| i32::try_from(value).ok()),
                    line:        related.line.and_then(|value| i32::try_from(value).ok()),
                    message:     related.message.clone(),
                    source_path: related.source_path.clone(),
                })
                .collect(),
            rule:        diagnostic.rule.clone(),
            severity:    match diagnostic.severity {
                Severity::Error => types::WorkflowDiagnosticSeverity::Error,
                Severity::Warning => types::WorkflowDiagnosticSeverity::Warning,
                Severity::Info => types::WorkflowDiagnosticSeverity::Info,
            },
            source_path: diagnostic.source_path.clone(),
            span_len:    diagnostic
                .span_len
                .and_then(|value| i64::try_from(value).ok()),
            span_start:  diagnostic
                .span_start
                .and_then(|value| i64::try_from(value).ok()),
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
    use lithos_llm::catalog::ProviderId;

    use super::*;

    /// Validate as the endpoints do: the structural pass, then Petri's check
    /// with the state's model client over `ready_providers`.
    fn validate_for_test(
        state: &AppState,
        prepared: &PreparedManifest,
        ready_providers: &[ProviderId],
    ) -> Result<Validated, WorkflowError> {
        let launch = petri_check::launch(
            &state.catalog(),
            &prepared.settings,
            ready_providers,
            None,
            None,
        );
        let runtime = crate::server::petri_runs::runtime_spec(state, ready_providers, false);
        validate_prepared_manifest(
            prepared,
            &HashMap::new(),
            launch,
            runtime,
            !ready_providers.is_empty(),
            false,
        )
    }

    /// Validate with every provider of the state's catalog eligible, as the
    /// legacy catalog validation judged a manifest.
    fn validate_with_catalog(
        state: &AppState,
        prepared: &PreparedManifest,
    ) -> Result<Validated, WorkflowError> {
        let providers = state
            .catalog()
            .enabled_provider_ids()
            .into_iter()
            .collect::<Vec<_>>();
        validate_for_test(state, prepared, &providers)
    }

    #[tokio::test]
    async fn ls_remote_capture_keeps_diagnostic_precedence_and_unlimited_output() {
        for (script, expected) in [
            ("printf stdout; printf stderr >&2; exit 7", "stderr"),
            ("printf stdout; exit 7", "stdout"),
        ] {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            assert_eq!(super::run_ls_remote(command).await.unwrap_err(), expected);
        }
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 200000 /dev/zero | tr '\\0' x >&2; exit 7"]);
        assert_eq!(
            super::run_ls_remote(command).await.unwrap_err().len(),
            200_000
        );
        let mut command = Command::new("sh");
        command.args(["-c", "exit 7"]);
        assert!(
            super::run_ls_remote(command)
                .await
                .unwrap_err()
                .starts_with("git ls-remote exited with status")
        );
    }

    fn minimal_manifest() -> types::RunManifest {
        types::RunManifest {
            args:      None,
            configs:   Vec::new(),
            cwd:       "/tmp/project".to_string(),
            git:       None,
            goal:      None,
            parent_id: None,
            title:     None,
            target:    types::ManifestTarget {
                path: "workflow.fabro".to_string(),
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

    fn environment_defaults_fixture() -> MergeMap<EnvironmentLayer> {
        MergeMap::from(HashMap::from([
            ("default".to_string(), EnvironmentLayer {
                provider: Some("local".to_string()),
                ..EnvironmentLayer::default()
            }),
            ("local".to_string(), EnvironmentLayer {
                provider: Some("local".to_string()),
                ..EnvironmentLayer::default()
            }),
            ("daytona".to_string(), EnvironmentLayer {
                provider: Some("daytona".to_string()),
                ..EnvironmentLayer::default()
            }),
            ("selected".to_string(), EnvironmentLayer {
                provider: Some("docker".to_string()),
                ..EnvironmentLayer::default()
            }),
            ("cloud".to_string(), EnvironmentLayer {
                provider: Some("daytona".to_string()),
                ..EnvironmentLayer::default()
            }),
        ]))
    }

    fn prepare_manifest(
        manifest_run_defaults: &RunLayer,
        manifest: &types::RunManifest,
    ) -> Result<PreparedManifest> {
        super::prepare_manifest_with_environment_defaults(
            manifest_run_defaults,
            &environment_defaults_fixture(),
            &HashMap::new(),
            manifest,
        )
    }

    fn openai_compatible_completion(model: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl_preflight",
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "OK"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
    }

    fn ready_moonshot_and_openrouter_state(
        server: &httpmock::MockServer,
    ) -> Arc<crate::server::AppState> {
        let moonshot_url = server.url("/moonshot/v1");
        let openrouter_url = server.url("/openrouter/v1");
        crate::test_support::TestAppStateBuilder::new()
            .llm_overlay_toml(&format!(
                r#"
[providers.moonshot]
base_url = "{moonshot_url}"

[providers.openrouter]
base_url = "{openrouter_url}"
enabled = true

"#
            ))
            .vault_entries([
                (EnvVars::KIMI_API_KEY, "test-moonshot-key"),
                (EnvVars::OPENROUTER_API_KEY, "test-openrouter-key"),
            ])
            .build()
    }

    async fn preflight_for_model(
        state: &Arc<crate::server::AppState>,
        model: &str,
    ) -> (types::PreflightResponse, bool) {
        let llm_result = state.resolve_llm_client().await;
        let mut ready_providers = llm_result
            .as_ref()
            .map(FabroClient::provider_ids)
            .unwrap_or_default();
        ready_providers.sort();
        assert_eq!(ready_providers, vec![
            ProviderId::new("moonshot"),
            ProviderId::new("openrouter")
        ]);

        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().source = format!(
            r#"
digraph Demo {{
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    work  [prompt="Do work", model="{model}"]
    start -> work -> exit
}}
"#
        );
        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_for_test(state, &prepared, &ready_providers).unwrap();
        assert!(!validated.has_errors(), "{:?}", validated.diagnostics());

        run_preflight(state.as_ref(), &prepared, &validated, llm_result)
            .await
            .unwrap()
    }

    /// Preflight for a workflow naming `model`, which Petri refuses.
    async fn preflight_for_refused_model(
        state: &Arc<crate::server::AppState>,
        model: &str,
    ) -> (types::PreflightResponse, bool) {
        let llm_result = state.resolve_llm_client().await;
        let ready_providers = llm_result
            .as_ref()
            .map(FabroClient::provider_ids)
            .unwrap_or_default();
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().source = format!(
            r#"
digraph Demo {{
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    work  [prompt="Do work", model="{model}"]
    start -> work -> exit
}}
"#
        );
        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_for_test(state, &prepared, &ready_providers).unwrap();
        assert!(validated.has_errors(), "{:?}", validated.diagnostics());

        run_preflight(state.as_ref(), &prepared, &validated, llm_result)
            .await
            .unwrap()
    }

    async fn resolve_and_run_preflight(
        state: &AppState,
        prepared: &PreparedManifest,
        validated: &Validated,
    ) -> Result<(types::PreflightResponse, bool)> {
        let llm_result = state.resolve_llm_client().await;
        run_preflight(state, prepared, validated, llm_result).await
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
            origin_url: origin_url.to_string(),
            branch:     branch.to_string(),
            sha:        None,
            dirty:      fabro_types::DirtyStatus::Clean,
        }
    }

    fn prepared_and_resolved_for_sandbox(
        provider: &SandboxProviderKind,
        clone_enabled: bool,
        git: Option<types::GitContext>,
    ) -> (PreparedManifest, RunNamespace) {
        let mut manifest = minimal_manifest();
        manifest.git = git;
        manifest.configs.push(types::ManifestConfig {
            path:   Some("/tmp/project/.fabro/project.toml".to_string()),
            source: Some(format!(
                r#"
_version = 1

[run.environment]
id = "selected"

[run.clone]
enabled = {clone_enabled}
"#
            )),
            type_:  types::ManifestConfigType::Project,
        });
        let mut environment_defaults = environment_defaults_fixture();
        environment_defaults.insert("selected".to_string(), EnvironmentLayer {
            provider: Some(provider.to_string()),
            ..EnvironmentLayer::default()
        });

        let prepared = super::prepare_manifest_with_environment_defaults(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &environment_defaults,
            &HashMap::new(),
            &manifest,
        )
        .unwrap();
        let resolved = prepared.settings.run.clone();

        (prepared, resolved)
    }

    #[test]
    fn docker_environment_cwd_is_reported_as_ignored() {
        let mut resolved = RunNamespace::default();
        resolved.environment.provider = SandboxProviderKind::DOCKER;
        resolved.environment.cwd = Some("/workspace/custom".to_string());

        assert_eq!(environment_capability_warnings(&resolved), vec![
            "docker provider ignores cwd".to_string()
        ]);
    }

    #[test]
    fn daytona_environment_cwd_is_reported_as_ignored() {
        let mut resolved = RunNamespace::default();
        resolved.environment.provider = SandboxProviderKind::DAYTONA;
        resolved.environment.cwd = Some("/home/daytona/workspace/custom".to_string());

        assert_eq!(environment_capability_warnings(&resolved), vec![
            "daytona provider ignores cwd".to_string()
        ]);
    }

    #[test]
    fn prepare_manifest_accepts_project_environment_catalog_definitions() {
        let mut manifest = minimal_manifest();
        manifest.configs.push(types::ManifestConfig {
            path:   Some(".fabro/project.toml".to_string()),
            source: Some(
                r#"_version = 1

[run.environment]
id = "cloud"

[environments.cloud]
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
        .expect("project environment catalog should resolve");

        assert_eq!(
            prepared.settings.run.environment.provider,
            SandboxProviderKind::LOCAL
        );
    }

    #[tokio::test]
    async fn repository_access_check_skips_when_clone_is_disabled() {
        let (prepared, resolved) = prepared_and_resolved_for_sandbox(
            &SandboxProviderKind::DOCKER,
            false,
            Some(git_context("https://github.com/acme/widgets", "main")),
        );
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_check = Arc::clone(&calls);
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            &SandboxProviderKind::DOCKER,
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
            &SandboxProviderKind::DOCKER,
            true,
            Some(git_context("https://gitlab.com/acme/widgets", "main")),
        );
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_check = Arc::clone(&calls);
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            &SandboxProviderKind::DOCKER,
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
            &SandboxProviderKind::DOCKER,
            true,
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
            &SandboxProviderKind::DOCKER,
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
            &SandboxProviderKind::DOCKER,
            true,
            Some(git_context("https://github.com/acme/widgets", "missing")),
        );
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            &SandboxProviderKind::DOCKER,
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
            &SandboxProviderKind::DOCKER,
            true,
            Some(git_context("https://github.com/acme/widgets", "main")),
        );

        let spec = preflight_sandbox_spec(
            &SandboxProviderKind::DOCKER,
            &prepared,
            &resolved,
            None,
            &ProviderAccess::default(),
        );

        let spec = spec.expect("Docker preflight sandbox spec");
        assert_eq!(spec.kind, SandboxProviderKind::DOCKER);
        assert!(spec.clone.skip);
        assert_eq!(
            spec.clone.origin_url.as_deref(),
            Some("https://github.com/acme/widgets")
        );
        assert_eq!(spec.clone.branch.as_deref(), Some("main"));
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
            preserve_sandbox: None,
            provider:         None,
            environment:      None,
            input:            Vec::new(),
            verbose:          None,
        });

        let prepared = prepare_manifest(&server_settings, &manifest).unwrap();

        assert_eq!(
            prepared.settings.run.execution.mode,
            fabro_types::settings::run::RunMode::DryRun
        );
    }

    #[test]
    fn prepare_manifest_applies_input_args_as_sparse_overrides() {
        let server_settings = manifest_run_defaults(Some(&server_settings_fixture(
            r#"
_version = 1

[run.inputs]
keep = "server"
override = "server"
"#,
        )));
        let mut manifest = minimal_manifest();
        manifest.args = Some(types::ManifestArgs {
            auto_approve:     None,
            dry_run:          None,
            label:            Vec::new(),
            model:            None,
            preserve_sandbox: None,
            provider:         None,
            environment:      None,
            input:            vec!["override=cli".to_string()],
            verbose:          None,
        });

        let prepared = prepare_manifest(&server_settings, &manifest).unwrap();

        assert_eq!(
            prepared.settings.run.inputs.get("keep"),
            Some(&toml::Value::String("server".to_string()))
        );
        assert_eq!(
            prepared.settings.run.inputs.get("override"),
            Some(&toml::Value::String("cli".to_string()))
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

        // run.prepare.steps replaces the whole list across layers, so the
        // higher-precedence workflow layer wins over cli.
        assert_eq!(prepared.settings.run.prepare.steps.len(), 1);
        assert_eq!(
            prepared.settings.run.prepare.steps[0].to_shell_command(),
            "workflow-setup"
        );
        assert!(settings_json.pointer("/server").is_none());
    }

    #[test]
    fn prepare_manifest_preserves_bundled_workflow_metadata() {
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().config =
            Some(types::ManifestWorkflowConfig {
                path:   "workflow.toml".to_string(),
                source: r#"
_version = 1

[workflow]
name = "Ship feature"
description = "Move the feature through review"

[workflow.metadata]
team = "platform"
priority = "high"

[run.environment]
id = "local"
"#
                .to_string(),
            });

        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();

        assert_eq!(
            prepared.settings.workflow.name.as_deref(),
            Some("Ship feature")
        );
        assert_eq!(
            prepared.settings.workflow.description.as_deref(),
            Some("Move the feature through review")
        );
        assert_eq!(
            prepared
                .settings
                .workflow
                .metadata
                .get("team")
                .map(String::as_str),
            Some("platform")
        );
        assert_eq!(
            prepared
                .settings
                .workflow
                .metadata
                .get("priority")
                .map(String::as_str),
            Some("high")
        );
    }

    #[test]
    fn prepare_manifest_keeps_missing_metadata_names_absent() {
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().source = r"
digraph GraphName {
    start [shape=Mdiamond]
    exit [shape=Msquare]
    start -> exit
}
"
        .to_string();
        manifest.configs.push(types::ManifestConfig {
            path:   Some(".fabro/project.toml".to_string()),
            source: Some(
                r#"_version = 1

[project.metadata]
team = "platform"
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

        assert_eq!(prepared.settings.workflow.name, None);
        assert_eq!(prepared.settings.project.name, None);
    }

    #[test]
    fn prepare_manifest_preserves_explicit_project_name() {
        let mut manifest = minimal_manifest();
        manifest.configs.push(types::ManifestConfig {
            path:   Some(".fabro/project.toml".to_string()),
            source: Some(
                r#"_version = 1

[project]
name = "Control Plane"
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

        assert_eq!(
            prepared.settings.project.name.as_deref(),
            Some("Control Plane")
        );
    }

    #[tokio::test]
    async fn invalid_preflight_returns_diagnostics_without_runtime_checks() {
        let state = crate::test_support::test_app_state();
        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &invalid_manifest(),
        )
        .unwrap();
        let validated = validate_with_catalog(&state, &prepared).unwrap();

        assert!(validated.has_errors());

        let (response, ok) = resolve_and_run_preflight(state.as_ref(), &prepared, &validated)
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
    async fn preflight_runs_github_token_check_when_run_level_permissions_declared() {
        // When a workflow declares `[run.integrations.github.permissions]`,
        // `run_github_token_check` is invoked and surfaces a "GitHub Token"
        // entry in the preflight report. With no configured GitHub App
        // credentials in the test fixture, the check status is
        // Warning/skipped — the important assertion is that the entry
        // *exists*, proving the gate now reads from run-level config.
        let state = crate::test_support::test_app_state();
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().config =
            Some(types::ManifestWorkflowConfig {
                path:   "workflow.toml".to_string(),
                source: r#"_version = 1

[run.environment]
id = "local"

[run.integrations.github.permissions]
issues = "read"
"#
                .to_string(),
            });

        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_with_catalog(&state, &prepared).unwrap();
        assert!(!validated.has_errors(), "{:?}", validated.diagnostics());

        let (response, _ok) = resolve_and_run_preflight(state.as_ref(), &prepared, &validated)
            .await
            .unwrap();

        assert!(
            response.checks.sections[0]
                .checks
                .iter()
                .any(|check| check.name == "GitHub Token"),
            "expected GitHub Token check to run when run-level permissions are set; \
             checks were {:?}",
            response.checks.sections[0]
                .checks
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn preflight_rejects_unresolved_github_permissions() {
        let state = crate::test_support::test_app_state();
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().config =
            Some(types::ManifestWorkflowConfig {
                path:   "workflow.toml".to_string(),
                source: r#"_version = 1

[run.environment]
id = "local"

[run.integrations.github.permissions]
issues = "{{ env.GITHUB_ISSUES_PERMISSION }}"
"#
                .to_string(),
            });

        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_with_catalog(&state, &prepared).unwrap();
        assert!(!validated.has_errors(), "{:?}", validated.diagnostics());

        let (response, ok) = resolve_and_run_preflight(state.as_ref(), &prepared, &validated)
            .await
            .unwrap();
        let github_token_check = response.checks.sections[0]
            .checks
            .iter()
            .find(|check| check.name == "GitHub Token")
            .expect("GitHub Token check should report invalid permissions");

        assert!(!ok);
        assert_eq!(
            github_token_check.status,
            types::PreflightCheckResultStatus::Error
        );
        assert_eq!(github_token_check.summary, "invalid permissions");
        assert!(
            github_token_check
                .remediation
                .as_deref()
                .is_some_and(|message| message.contains("GITHUB_ISSUES_PERMISSION"))
        );
    }

    #[tokio::test]
    async fn preflight_allows_pull_request_enabled_without_github_credentials() {
        let state = crate::test_support::test_app_state();
        let source_dir = tempfile::tempdir().unwrap();
        let mut manifest = minimal_manifest();
        manifest.cwd = source_dir.path().to_string_lossy().into_owned();
        manifest.configs.push(types::ManifestConfig {
            path:   Some("/tmp/project/.fabro/project.toml".to_string()),
            source: Some(
                r#"
_version = 1

[run.pull_request]
enabled = true

[run.environment]
id = "local"
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
        let validated = validate_with_catalog(&state, &prepared).unwrap();

        assert!(!validated.has_errors(), "{:?}", validated.diagnostics());

        let (response, ok) = resolve_and_run_preflight(state.as_ref(), &prepared, &validated)
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

    #[test]
    fn prepare_manifest_does_not_backfill_missing_project_and_workflow_names() {
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().config =
            Some(types::ManifestWorkflowConfig {
                path:   "workflow.toml".to_string(),
                source: "_version = 1\n".to_string(),
            });
        manifest.configs.push(types::ManifestConfig {
            path:   Some("/tmp/project/.fabro/project.toml".to_string()),
            source: Some("_version = 1\n".to_string()),
            type_:  types::ManifestConfigType::Project,
        });

        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();

        assert_eq!(prepared.settings.project.name, None);
        assert_eq!(prepared.settings.workflow.name, None);
    }

    #[test]
    fn prepare_manifest_preserves_explicit_project_and_workflow_names() {
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().config =
            Some(types::ManifestWorkflowConfig {
                path:   "workflow.toml".to_string(),
                source: r#"
_version = 1

[workflow]
name = "Workflow Config Name"
"#
                .to_string(),
            });
        manifest.configs.push(types::ManifestConfig {
            path:   Some("/tmp/project/.fabro/project.toml".to_string()),
            source: Some(
                r#"
_version = 1

[project]
name = "Project Config Name"
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

        assert_eq!(
            prepared.settings.project.name.as_deref(),
            Some("Project Config Name")
        );
        assert_eq!(
            prepared.settings.workflow.name.as_deref(),
            Some("Workflow Config Name")
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

[run.environment]
id = "daytona"
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
        let validated = validate_with_catalog(&state, &prepared).unwrap();

        let (response, _ok) = resolve_and_run_preflight(state.as_ref(), &prepared, &validated)
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
        let state = crate::test_support::TestAppStateBuilder::new()
            .runtime_settings(
                crate::test_support::default_test_server_settings(),
                RunLayer::default(),
            )
            .max_concurrent_runs(5)
            .provider_base_url("openai", server.url("/v1"))
            .build();
        state
            .stores
            .vault
            .set(
                "OPENAI_API_KEY",
                "test-openai-key",
                fabro_vault::SecretType::Token,
                None,
            )
            .await
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
        let validated = validate_with_catalog(&state, &prepared).unwrap();

        let (response, ok) = resolve_and_run_preflight(state.as_ref(), &prepared, &validated)
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
                .contains("quota limited")
        );
        assert!(response_mock.calls_async().await >= 1);
    }

    #[tokio::test]
    async fn preflight_uses_ready_providers_for_known_shared_alias() {
        let server = httpmock::MockServer::start_async().await;
        let openrouter_probe = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/openrouter/v1/chat/completions")
                    .header("authorization", "Bearer test-openrouter-key")
                    .json_body_includes(r#"{"model":"anthropic/claude-fable-5"}"#);
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(openai_compatible_completion("anthropic/claude-fable-5"));
            })
            .await;
        let state = ready_moonshot_and_openrouter_state(&server);

        let (response, _ok) = preflight_for_model(&state, "claude-fable").await;

        let llm_check = response.checks.sections[0]
            .checks
            .iter()
            .find(|check| check.name == "LLM" && check.summary == "claude-fable-5")
            .unwrap_or_else(|| {
                panic!(
                    "preflight should include Claude Fable: {:?}",
                    response.checks.sections
                )
            });
        assert_eq!(
            llm_check
                .details
                .iter()
                .map(|detail| detail.text.as_str())
                .find(|detail| detail.starts_with("Provider: ")),
            Some("Provider: openrouter")
        );
        assert_eq!(llm_check.status, types::PreflightCheckResultStatus::Pass);
        openrouter_probe.assert_async().await;
    }

    #[tokio::test]
    async fn preflight_uses_ready_providers_for_unknown_unqualified_model() {
        let server = httpmock::MockServer::start_async().await;
        let moonshot_probe = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/moonshot/v1/chat/completions")
                    .header("authorization", "Bearer test-moonshot-key")
                    .json_body_includes(r#"{"model":"provider-private-preview"}"#);
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(openai_compatible_completion("provider-private-preview"));
            })
            .await;
        let state = ready_moonshot_and_openrouter_state(&server);

        // Petri admits no model its catalog lacks, so the workflow is refused
        // before any probe runs: no passthrough of an unknown model.
        let (response, ok) = preflight_for_refused_model(&state, "provider-private-preview").await;

        assert!(!ok);
        assert!(response.workflow.diagnostics.iter().any(|diagnostic| {
            diagnostic.rule == "attractor.model.unknown"
                && diagnostic.message.contains("provider-private-preview")
        }));
        assert!(
            response.checks.sections[0]
                .checks
                .iter()
                .all(|check| check.name != "LLM"),
            "no LLM check runs on a refused workflow"
        );
        assert_eq!(moonshot_probe.calls_async().await, 0);
    }

    #[test]
    fn static_validation_rejects_unknown_llm_provider() {
        let state = crate::test_support::test_app_state();
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().source = r#"
digraph Demo {
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    work  [prompt="Do work", model="missing-model", provider="missing-provider"]
    start -> work -> exit
}
"#
        .to_string();
        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let validated = validate_with_catalog(&state, &prepared).unwrap();

        // Petri refuses the model node: the provider is not in the catalog.
        let refusal = validated
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.severity == Severity::Error)
            .expect("unknown provider should fail static validation");
        assert_eq!(refusal.rule, "attractor.model.unknown", "{refusal:?}");
        assert!(refusal.message.contains("missing-provider"), "{refusal:?}");
    }

    #[tokio::test]
    async fn preflight_resolves_model_aliases_from_app_state_catalog() {
        let state = crate::test_support::TestAppStateBuilder::new()
            .llm_overlay_toml(
                r#"
[providers.acme]
display_name = "Acme"
base_url = "https://api.acme.test/v1"
auth = { type = "bearer" }
default_model = "acme-large"

[providers.acme.metadata.agent]
profile = "openai"

[providers.acme.models."acme-large"]
display_name = "Acme Large"
aliases = ["vl"]
api_model = "acme-large"
limits = { context_tokens = 128000, max_output_tokens = 8192 }
capabilities = { text = true, tools = true }
"#,
            )
            .build();
        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().source = r#"
digraph Demo {
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    work  [prompt="Do work", model="vl"]
    start -> work -> exit
}
"#
        .to_string();
        let prepared = prepare_manifest(
            &manifest_run_defaults(Some(&default_settings_fixture())),
            &manifest,
        )
        .unwrap();
        let llm_result = state.resolve_llm_client().await;
        let ready_providers = llm_result
            .as_ref()
            .map(FabroClient::provider_ids)
            .unwrap_or_default();
        assert!(ready_providers.is_empty());

        // With no provider ready there is no model client to resolve the
        // alias against, so Fabro refuses the model node before any check
        // runs: the run could not pick a model at create either.
        let validated = validate_for_test(&state, &prepared, &ready_providers).unwrap();
        assert!(validated.has_errors());
        assert!(
            validated
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.rule == petri_check::NO_READY_PROVIDER_RULE),
            "{:?}",
            validated.diagnostics()
        );

        let (response, ok) = run_preflight(state.as_ref(), &prepared, &validated, llm_result)
            .await
            .unwrap();
        assert!(!ok);
        assert!(
            response
                .checks
                .sections
                .iter()
                .flat_map(|section| section.checks.iter())
                .all(|check| check.name != "LLM"),
            "no LLM check runs on a refused workflow"
        );
    }

    mod settings_layer_with_resolved_dockerfiles_tests {
        //! `settings_layer_with_resolved_dockerfiles` parses bundled
        //! workflow.toml through the strict `SettingsLayer` schema, so
        //! unknown fields anywhere in the document trip
        //! `deny_unknown_fields`.

        use fabro_config::parse::SettingsSource;
        use fabro_types::ManifestPath;
        use fabro_workflow::workflow_bundle::{BundledWorkflow, ParsedWorkflowConfig};

        use super::super::settings_layer_with_resolved_dockerfiles;

        fn workflow_with_config(source: &str) -> BundledWorkflow {
            BundledWorkflow {
                path:   ManifestPath::from_wire("workflow.fabro").expect("path should be valid"),
                source: "digraph G {}".to_string(),
                config: Some(ParsedWorkflowConfig {
                    path:   ManifestPath::from_wire("workflow.toml")
                        .expect("config path should be valid"),
                    source: source.to_string(),
                }),
                files:  std::collections::HashMap::new(),
            }
        }

        #[test]
        fn parses_run_integrations_github_permissions() {
            let workflow = workflow_with_config(
                r#"_version = 1

[run.integrations.github.permissions]
issues = "read"
"#,
            );

            let layer = settings_layer_with_resolved_dockerfiles(
                &workflow.config.as_ref().unwrap().source,
                &workflow.config.as_ref().unwrap().path,
                &workflow.files,
                SettingsSource::Workflow,
            )
            .expect("workflow.toml should parse");
            let run = layer.run.expect("run layer should be present");
            let github = run
                .integrations
                .as_ref()
                .and_then(|integrations| integrations.github.as_ref())
                .expect("integrations.github should be present");
            let permissions = github
                .permissions
                .as_ref()
                .expect("permissions should be present");
            assert_eq!(permissions.len(), 1);
            assert!(permissions.contains_key("issues"));
        }

        #[test]
        fn rejects_stale_server_integrations_github_permissions() {
            let workflow = workflow_with_config(
                r#"_version = 1

[server.integrations.github.permissions]
issues = "read"
"#,
            );

            let err = settings_layer_with_resolved_dockerfiles(
                &workflow.config.as_ref().unwrap().source,
                &workflow.config.as_ref().unwrap().path,
                &workflow.files,
                SettingsSource::Workflow,
            )
            .expect_err("stale [server.integrations.github.permissions] should be rejected");
            let message = format!("{err:#}");
            assert!(
                message.contains("permissions") || message.contains("unknown field"),
                "expected unknown-field error, got: {message}"
            );
        }

        #[test]
        fn accepts_workflow_block_and_version() {
            let workflow = workflow_with_config(
                r#"_version = 1

[workflow]
name = "demo"

[run.integrations.github.permissions]
contents = "read"
"#,
            );

            let layer = settings_layer_with_resolved_dockerfiles(
                &workflow.config.as_ref().unwrap().source,
                &workflow.config.as_ref().unwrap().path,
                &workflow.files,
                SettingsSource::Workflow,
            )
            .expect("workflow + run blocks should parse");
            let run = layer.run.expect("run layer should be present");
            assert!(run.integrations.is_some());
        }

        #[test]
        fn inlines_dockerfile_path_in_environments_catalog() {
            use fabro_config::EnvironmentDockerfileLayer;

            let source = r#"_version = 1

[environments.fabro-dev]
provider = "daytona"

[environments.fabro-dev.image]
dockerfile = { path = "Dockerfile" }
"#;
            let config_path =
                ManifestPath::from_wire("project.toml").expect("config path should be valid");
            let mut files = std::collections::HashMap::new();
            files.insert(
                ManifestPath::from_wire("Dockerfile").expect("dockerfile path should be valid"),
                "FROM ubuntu:24.04\n".to_string(),
            );

            let layer = settings_layer_with_resolved_dockerfiles(
                source,
                &config_path,
                &files,
                SettingsSource::Project,
            )
            .expect("project config should parse");

            let dockerfile = layer
                .environments
                .get("fabro-dev")
                .expect("fabro-dev environment should be present")
                .image
                .as_ref()
                .and_then(|image| image.dockerfile.as_ref())
                .expect("dockerfile should be present");
            assert_eq!(
                *dockerfile,
                EnvironmentDockerfileLayer::Inline("FROM ubuntu:24.04\n".to_string()),
                "catalog dockerfile path should be inlined, got: {dockerfile:?}"
            );
        }
    }

    mod github_additional_repository_checks {
        //! Seam-injected tests for the declared-additional-repositories
        //! preflight path: one scoped mint, per-repository probes with
        //! deterministic primary-first reporting, GH_TOKEN warning, and the
        //! replication-lag retry policy.

        use std::sync::Mutex as StdMutex;
        use std::sync::atomic::{AtomicU64, Ordering};

        use fabro_github::token_source::{
            ResolvedToken, SecretString, TokenProvenance, TokenSnapshot,
        };
        use fabro_types::settings::run::RunIntegrationsGithubSettings;

        use super::*;

        fn static_token(secret: &str) -> ResolvedToken {
            ResolvedToken {
                token:          SecretString::new(secret.to_string()),
                snapshot:       TokenSnapshot {
                    generation: 0,
                    provenance: TokenProvenance::Static,
                },
                refresh_failed: false,
            }
        }

        fn fresh_minted_snapshot() -> TokenSnapshot {
            let now = chrono::Utc::now();
            TokenSnapshot {
                generation: 1,
                provenance: TokenProvenance::Minted {
                    minted_at:  now,
                    expires_at: now + chrono::Duration::minutes(60),
                },
            }
        }

        fn declared(origin: &str, additional: &[&str]) -> (PreparedManifest, RunNamespace) {
            let (prepared, mut resolved) = prepared_and_resolved_for_sandbox(
                &SandboxProviderKind::LOCAL,
                true,
                Some(git_context(origin, "main")),
            );
            resolved.integrations.github = RunIntegrationsGithubSettings {
                permissions:             HashMap::from([(
                    "contents".to_string(),
                    InterpString::parse("read"),
                )]),
                additional_repositories: additional
                    .iter()
                    .map(|value| value.parse().expect("test slug should parse"))
                    .collect(),
            };
            (prepared, resolved)
        }

        fn pat_creds() -> fabro_github::GitHubCredentials {
            fabro_github::GitHubCredentials::Pat("ghp_test".to_string())
        }

        #[tokio::test(start_paused = true)]
        async fn reports_each_repository_primary_first_despite_probe_completion_order() {
            let (prepared, resolved) = declared("https://github.com/acme/widgets", &[
                "acme/zeta",
                "acme/alpha",
            ]);
            let minted = Arc::new(StdMutex::new(Vec::new()));
            let minted_for_seam = Arc::clone(&minted);
            let mut checks = Vec::new();

            let ok = run_github_token_check_with(
                &mut checks,
                &prepared,
                &resolved,
                Some(pat_creds()),
                move |access, _creds| {
                    minted_for_seam.lock().unwrap().push(access);
                    async { Ok(static_token("scoped-token")) }
                },
                |slug, _token| async move {
                    // Invert completion order: the primary finishes last.
                    let delay = match slug.repo() {
                        "widgets" => 30,
                        "alpha" => 20,
                        _ => 10,
                    };
                    time::sleep(Duration::from_millis(delay)).await;
                    Ok(())
                },
            )
            .await;

            assert!(ok);
            // One mint listing every repository with the shared permissions.
            let minted = minted.lock().unwrap();
            assert_eq!(minted.len(), 1);
            assert_eq!(minted[0].repository_names(), vec![
                "widgets", "alpha", "zeta"
            ]);
            assert_eq!(
                minted[0].permissions().get("contents").map(String::as_str),
                Some("read")
            );

            let names: Vec<&str> = checks.iter().map(|check| check.name.as_str()).collect();
            assert_eq!(names, vec![
                "GitHub Token",
                "GitHub Repository (acme/widgets)",
                "GitHub Repository (acme/alpha)",
                "GitHub Repository (acme/zeta)",
            ]);
            assert!(checks.iter().all(|check| check.status == CheckStatus::Pass));
            // The token never reaches check output.
            for check in &checks {
                let rendered = format!("{check:?}");
                assert!(!rendered.contains("scoped-token"), "{rendered}");
            }
        }

        #[tokio::test]
        async fn installation_resolution_failure_names_only_the_inaccessible_repository() {
            let (prepared, resolved) =
                declared("https://github.com/acme/widgets", &["acme/keystone"]);
            let probes = Arc::new(AtomicU64::new(0));
            let probes_for_seam = Arc::clone(&probes);
            let mut checks = Vec::new();

            let ok = run_github_token_check_with(
                &mut checks,
                &prepared,
                &resolved,
                Some(pat_creds()),
                |_access, _creds| async {
                    Err(
                        "the GitHub App installation cannot see repository acme/keystone; add \
                         it to the installation's repository access"
                            .to_string(),
                    )
                },
                move |_slug, _token| {
                    probes_for_seam.fetch_add(1, Ordering::SeqCst);
                    async { Ok(()) }
                },
            )
            .await;

            assert!(!ok);
            assert_eq!(
                probes.load(Ordering::SeqCst),
                0,
                "no probes after a failed mint"
            );
            assert_eq!(checks.last().unwrap().name, "GitHub Token");
            assert_eq!(checks.last().unwrap().status, CheckStatus::Error);
            let remediation = checks.last().unwrap().remediation.as_deref().unwrap();
            assert!(remediation.contains("acme/keystone"), "{remediation}");
            assert!(!remediation.contains("acme/widgets"), "{remediation}");
        }

        #[tokio::test]
        async fn successful_mint_with_failed_probe_still_fails() {
            let (prepared, resolved) =
                declared("https://github.com/acme/widgets", &["acme/keystone"]);
            let mut checks = Vec::new();

            let ok = run_github_token_check_with(
                &mut checks,
                &prepared,
                &resolved,
                Some(pat_creds()),
                |_access, _creds| async { Ok(static_token("scoped-token")) },
                |slug, _token| async move {
                    if slug.repo() == "keystone" {
                        Err("remote: Repository not found.".to_string())
                    } else {
                        Ok(())
                    }
                },
            )
            .await;

            assert!(!ok);
            let keystone = checks
                .iter()
                .find(|check| check.name == "GitHub Repository (acme/keystone)")
                .expect("keystone probe result should be reported");
            assert_eq!(keystone.status, CheckStatus::Error);
            assert!(
                !keystone
                    .remediation
                    .as_deref()
                    .unwrap_or_default()
                    .contains("scoped-token")
            );
            let widgets = checks
                .iter()
                .find(|check| check.name == "GitHub Repository (acme/widgets)")
                .expect("primary probe result should be reported");
            assert_eq!(widgets.status, CheckStatus::Pass);
        }

        #[tokio::test]
        async fn resolved_gh_token_warns_without_failing() {
            let (prepared, mut resolved) =
                declared("https://github.com/acme/widgets", &["acme/keystone"]);
            resolved
                .environment
                .env
                .insert("GH_TOKEN".to_string(), InterpString::parse("user-token"));
            let mut checks = Vec::new();

            let ok = run_github_token_check_with(
                &mut checks,
                &prepared,
                &resolved,
                Some(pat_creds()),
                |_access, _creds| async { Ok(static_token("scoped-token")) },
                |_slug, _token| async { Ok(()) },
            )
            .await;

            assert!(ok, "a GH_TOKEN override warns but does not fail preflight");
            let warning = checks
                .iter()
                .find(|check| check.name == "GH_TOKEN Override")
                .expect("GH_TOKEN warning should be reported");
            assert_eq!(warning.status, CheckStatus::Warning);
        }

        #[tokio::test]
        async fn missing_origin_fails_for_declared_repositories() {
            let (prepared, resolved) = {
                let (mut prepared, resolved) =
                    declared("https://github.com/acme/widgets", &["acme/keystone"]);
                prepared.git = None;
                (prepared, resolved)
            };
            let mut checks = Vec::new();

            let ok = run_github_token_check_with(
                &mut checks,
                &prepared,
                &resolved,
                Some(pat_creds()),
                |_access, _creds| async { Ok(static_token("scoped-token")) },
                |_slug, _token| async { Ok(()) },
            )
            .await;

            assert!(!ok);
            let check = checks.last().unwrap();
            assert_eq!(check.summary, "missing origin");
            assert!(
                check
                    .remediation
                    .as_deref()
                    .unwrap_or_default()
                    .contains("requires a GitHub run origin"),
                "{:?}",
                check.remediation
            );
        }

        #[tokio::test]
        async fn missing_credentials_fail_for_declared_repositories() {
            let (prepared, resolved) =
                declared("https://github.com/acme/widgets", &["acme/keystone"]);
            let mut checks = Vec::new();

            let ok = run_github_token_check_with(
                &mut checks,
                &prepared,
                &resolved,
                None,
                |_access, _creds| async { Ok(static_token("scoped-token")) },
                |_slug, _token| async { Ok(()) },
            )
            .await;

            assert!(!ok);
            assert_eq!(checks.last().unwrap().summary, "missing credentials");
        }

        #[tokio::test(start_paused = true)]
        async fn replication_lag_failure_retries_with_the_same_token_and_succeeds() {
            let attempts = Arc::new(AtomicU64::new(0));
            let attempts_for_run = Arc::clone(&attempts);

            let result = probe_with_replication_retry(fresh_minted_snapshot(), move || {
                let attempts = Arc::clone(&attempts_for_run);
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err("remote: Repository not found.".to_string())
                    } else {
                        Ok(())
                    }
                }
            })
            .await;

            assert!(result.is_ok());
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
        }

        #[tokio::test(start_paused = true)]
        async fn static_credential_auth_failures_do_not_retry() {
            let attempts = Arc::new(AtomicU64::new(0));
            let attempts_for_run = Arc::clone(&attempts);
            let static_snapshot = TokenSnapshot {
                generation: 0,
                provenance: TokenProvenance::Static,
            };

            let result = probe_with_replication_retry(static_snapshot, move || {
                let attempts = Arc::clone(&attempts_for_run);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err("remote: Repository not found.".to_string())
                }
            })
            .await;

            assert!(result.is_err());
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                1,
                "a 404 with a static credential cannot become valid by waiting"
            );
        }
    }
}
