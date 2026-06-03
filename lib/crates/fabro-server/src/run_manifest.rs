use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use fabro_api::types;
use fabro_auth::auth_issue_message;
use fabro_config::parse::{self, SettingsSource};
use fabro_config::{
    CliLayer, CliOutputLayer, EnvironmentDockerfileLayer, EnvironmentImageLayer, EnvironmentLayer,
    MergeMap, RunLayer, SettingsLayer, WorkflowSettingsBuilder, parse_input_overrides,
    parse_labels, project,
};
use fabro_graphviz::graph::{Graph, is_llm_handler_type};
use fabro_graphviz::render::apply_direction;
use fabro_llm::model_test::{ModelTestStatus, run_basic_model_probe};
use fabro_model::{Catalog, ProviderId};
use fabro_sandbox::daytona::DaytonaConfig;
use fabro_sandbox::from_environment::{
    daytona_config_from_environment, docker_config_from_environment,
};
use fabro_sandbox::redact::redact_auth_url;
use fabro_sandbox::{DockerSandboxOptions, Sandbox, SandboxSpec};
use fabro_static::EnvVars;
use fabro_types::settings::cli::OutputVerbosity;
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::run::{EnvironmentProvider, RunGoal, RunNamespace};
use fabro_types::{ManifestPath, RunId, SandboxProviderKind, ServerSettings, WorkflowSettings};
use fabro_util::check_report::{CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus};
use fabro_validate::Severity;
use fabro_workflow::Error as WorkflowError;
use fabro_workflow::operations::{CreateRunInput, ValidateInput, WorkflowInput, validate};
use fabro_workflow::pipeline::Validated;
use fabro_workflow::run_materialization::materialize_run;
use fabro_workflow::workflow_bundle::{BundledWorkflow, ParsedWorkflowConfig, WorkflowBundle};
use futures_util::stream::{self, StreamExt};
use tokio::process::Command;
use tokio::time;
use tracing::warn;

use crate::interp::process_env_var;
use crate::server::AppState;
use crate::server_secrets::LlmClientResult;

#[derive(Clone)]
pub(crate) struct PreparedManifest {
    pub cwd:              PathBuf,
    pub git:              Option<types::GitContext>,
    pub root_source:      String,
    pub run_id:           Option<RunId>,
    pub parent_id:        Option<RunId>,
    pub title:            Option<String>,
    pub settings:         WorkflowSettings,
    pub target_path:      ManifestPath,
    pub workflow_bundle:  WorkflowBundle,
    pub workflow_input:   BundledWorkflow,
    pub source_directory: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct ManifestSettingsOverrides {
    run:             Option<RunLayer>,
    cli:             Option<CliLayer>,
    input_overrides: HashMap<String, toml::Value>,
}

#[cfg(test)]
pub(crate) fn manifest_run_defaults(run: Option<&RunLayer>) -> RunLayer {
    run.cloned().unwrap_or_default()
}

pub(crate) fn prepare_manifest_with_environment_defaults(
    manifest_run_defaults: &RunLayer,
    manifest_environment_defaults: &MergeMap<EnvironmentLayer>,
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
    let mut workflow_settings_builder = WorkflowSettingsBuilder::new().server_manifest_defaults(
        manifest_run_defaults.clone(),
        manifest_environment_defaults.clone(),
    );
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
    let title = manifest
        .title
        .as_ref()
        .map(|title| fabro_types::normalize_explicit_run_title(title.as_str()))
        .transpose()?;

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
        parent_id: manifest
            .parent_id
            .as_deref()
            .map(str::parse::<RunId>)
            .transpose()
            .context("invalid parent run ID")?,
        title,
        settings: settings.clone(),
        target_path,
        workflow_bundle,
        workflow_input,
        source_directory: project::resolve_working_directory_from_run(&settings.run, &cwd),
    })
}

pub(crate) fn validate_prepared_manifest(
    prepared: &PreparedManifest,
    catalog: Arc<Catalog>,
) -> Result<Validated, WorkflowError> {
    validate(ValidateInput {
        workflow: WorkflowInput::Bundled(prepared.workflow_input.clone()),
        settings: prepared.settings.clone(),
        cwd: prepared.cwd.clone(),
        custom_transforms: Vec::new(),
        catalog,
    })
}

pub(crate) fn create_run_input(
    prepared: PreparedManifest,
    configured_providers: Vec<ProviderId>,
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
        title: prepared.title,
        automation: None,
        git: prepared.git,
        fork_source_ref: None,
        parent_id: prepared.parent_id,
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

fn settings_layer_with_resolved_dockerfiles(
    source: &str,
    config_path: &ManifestPath,
    files: &HashMap<ManifestPath, String>,
    settings_source: SettingsSource,
) -> Result<SettingsLayer> {
    // Parse via `SettingsLayer` so unknown nested keys (like a stale
    // `[server.integrations.github.permissions]` after the move to
    // `[run.integrations.github.permissions]`) trip `deny_unknown_fields`.
    let mut layer = source
        .parse::<SettingsLayer>()
        .context("Failed to parse run config TOML")?;
    parse::validate_settings_source(&layer, settings_source)
        .context("Failed to parse run config TOML")?;
    resolve_manifest_dockerfiles(&mut layer, config_path, files)?;
    Ok(layer)
}

fn manifest_args_overrides(
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
        docker_image:     args.docker_image.as_deref(),
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

fn resolve_manifest_dockerfiles(
    layer: &mut SettingsLayer,
    config_path: &ManifestPath,
    files: &HashMap<ManifestPath, String>,
) -> Result<()> {
    for environment in layer.environments.values_mut() {
        if let Some(image) = environment.image.as_mut() {
            resolve_manifest_dockerfile(image, config_path, files)?;
        }
    }
    if let Some(image) = layer
        .run
        .as_mut()
        .and_then(|run| run.environment.as_mut())
        .and_then(|environment| environment.image.as_mut())
    {
        resolve_manifest_dockerfile(image, config_path, files)?;
    }
    Ok(())
}

fn resolve_manifest_dockerfile(
    image: &mut EnvironmentImageLayer,
    config_path: &ManifestPath,
    files: &HashMap<ManifestPath, String>,
) -> Result<()> {
    let source = image.dockerfile.as_mut();
    let Some(source) = source else {
        return Ok(());
    };
    let EnvironmentDockerfileLayer::Path { path } = &*source else {
        return Ok(());
    };
    let path_owned = path.clone();
    let manifest_path = ManifestPath::from_reference(config_path.parent_or_dot(), &path_owned)
        .ok_or_else(|| anyhow!("unsupported dockerfile reference: {path_owned}"))?;
    let content = files
        .get(&manifest_path)
        .cloned()
        .ok_or_else(|| anyhow!("missing bundled dockerfile: {manifest_path}"))?;
    *source = EnvironmentDockerfileLayer::Inline(content);
    Ok(())
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
    let llm_result = state.resolve_llm_client().await;
    let configured_providers = match &llm_result {
        Ok(result) => result.provider_ids(),
        Err(err) => {
            warn!(error = ?err, "Failed to resolve LLM client while checking ready providers");
            Vec::new()
        }
    };
    let materialized = materialize_run(
        prepared.settings.clone(),
        graph,
        catalog.as_ref(),
        &configured_providers,
    );
    let resolved_run = materialized.run;
    let server_settings = state.server_settings();
    let github_integration = &server_settings.server.integrations.github;
    let sandbox_provider = effective_sandbox_provider(&resolved_run);
    if let Some(error) = sandbox_provider_policy_error(&server_settings, sandbox_provider) {
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
    let needs_github_credentials =
        sandbox_provider.is_clone_based() || resolved_run.integrations.github.is_token_requested();
    let github_app = if needs_github_credentials {
        state
            .github_credentials(github_integration)
            .unwrap_or_default()
    } else {
        None
    };

    let daytona_api_key = state.vault_secret(EnvVars::DAYTONA_API_KEY);
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
        &mut checks,
        graph,
        &resolved_run,
        &configured_providers,
        catalog.as_ref(),
        llm_result,
    )
    .await;
    run_github_token_check(&mut checks, prepared, &resolved_run, github_app).await;

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

pub(crate) fn sandbox_provider_policy_error(
    server_settings: &ServerSettings,
    provider: SandboxProviderKind,
) -> Option<String> {
    let enabled = server_settings
        .server
        .sandbox
        .providers
        .for_provider(provider)
        .enabled;
    (!enabled).then(|| {
        format!(
            "sandbox provider \"{provider}\" is disabled by server.sandbox.providers.{provider}.enabled"
        )
    })
}

pub(crate) fn effective_sandbox_provider(settings: &RunNamespace) -> SandboxProviderKind {
    SandboxProviderKind::from(settings.environment.provider).effective_for(settings.execution.mode)
}

fn resolve_daytona_config(settings: &RunNamespace) -> DaytonaConfig {
    daytona_config_from_environment(&settings.environment, !settings.clone.enabled)
}

fn resolve_docker_config(settings: &RunNamespace) -> DockerSandboxOptions {
    docker_config_from_environment(&settings.environment, !settings.clone.enabled)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitRemoteRefCheck {
    origin_url: String,
    branch:     Option<String>,
}

fn clone_disabled_for_provider(provider: SandboxProviderKind, resolved_run: &RunNamespace) -> bool {
    match provider {
        SandboxProviderKind::Docker | SandboxProviderKind::Daytona => !resolved_run.clone.enabled,
        SandboxProviderKind::Local => false,
    }
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
    match environment.provider {
        EnvironmentProvider::Local => {
            if environment.resources.cpu.is_some()
                || environment.resources.memory.is_some()
                || environment.resources.disk.is_some()
            {
                warnings.push("local provider ignores resource limits".to_string());
            }
            if !environment.volumes.is_empty() {
                warnings.push("local provider ignores volume mounts".to_string());
            }
            if !environment.labels.is_empty() {
                warnings.push("local provider ignores labels".to_string());
            }
            if environment.lifecycle.auto_stop.is_some() {
                warnings.push("local provider ignores lifecycle.auto_stop".to_string());
            }
        }
        EnvironmentProvider::Docker => {
            if environment.resources.disk.is_some() {
                warnings.push("docker provider ignores disk resource limits".to_string());
            }
            if !environment.volumes.is_empty() {
                warnings.push("docker provider ignores volume mounts".to_string());
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
        EnvironmentProvider::Daytona => {}
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
    sandbox_provider: SandboxProviderKind,
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
    sandbox_provider: SandboxProviderKind,
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
    sandbox_provider: SandboxProviderKind,
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
        SandboxProviderKind::Local => SandboxSpec::Local {
            working_directory: prepared.source_directory.clone(),
        },
        SandboxProviderKind::Docker => {
            let mut config = resolve_docker_config(resolved_run);
            config.skip_clone = true;
            SandboxSpec::Docker {
                config,
                github_app,
                run_id: None,
                clone_origin_url,
                clone_branch,
            }
        }
        SandboxProviderKind::Daytona => {
            let mut config = resolve_daytona_config(resolved_run);
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
    sandbox_provider: SandboxProviderKind,
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
        if matches!(sandbox_provider, SandboxProviderKind::Daytona) {
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

const MODEL_PREFLIGHT_PROBE_CONCURRENCY: usize = 4;

struct PendingModelProbe {
    index:         usize,
    model_id:      String,
    provider_name: String,
}

async fn run_llm_check(
    checks: &mut Vec<CheckResult>,
    graph: &Graph,
    settings: &RunNamespace,
    configured_providers: &[ProviderId],
    catalog: &Catalog,
    llm_result: Result<LlmClientResult>,
) -> bool {
    let (model, provider) = resolve_model_provider(settings, graph, configured_providers, catalog);
    let default_provider = provider.as_deref().unwrap_or("anthropic");
    let mut model_providers = std::collections::BTreeSet::new();
    let mut has_llm_nodes = false;

    for node in graph.nodes.values() {
        if !is_llm_handler_type(node.handler_type()) {
            continue;
        }
        has_llm_nodes = true;
        let node_model = node.model().unwrap_or(&model);
        let node_provider = node.provider().unwrap_or(default_provider);
        let (resolved_model, resolved_provider) = if let Some(info) = catalog.get(node_model) {
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

    match llm_result {
        Ok(result) => {
            let auth_issues = result.auth_issues;
            let registration_issues = result.registration_issues;
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
                        remediation: Some(auth_issue_message(&provider_id, issue)),
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
                        remediation: Some(issue.error.to_string()),
                    }));
                } else if !client.has_provider(provider_name) {
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
                        let outcome =
                            run_basic_model_probe(&probe.model_id, &probe.provider_name, client)
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
    let provider_id = ProviderId::from(provider_name);
    catalog
        .provider(&provider_id)
        .map_or(provider_id, |provider| provider.id.clone())
}

#[expect(
    clippy::disallowed_methods,
    reason = "raw source is today's behavior; run.model.name/provider are slated for demotion to plain String in the \
              interpolation unification (D2)"
)]
fn resolve_model_provider(
    settings: &RunNamespace,
    _graph: &Graph,
    configured_providers: &[ProviderId],
    catalog: &Catalog,
) -> (String, Option<String>) {
    let provider = settings
        .model
        .provider
        .as_ref()
        .map(InterpString::as_source);
    let model = settings.model.name.as_ref().map_or_else(
        || {
            catalog
                .default_for_configured_ids(configured_providers)
                .id
                .clone()
        },
        InterpString::as_source,
    );

    match catalog.get(&model) {
        Some(info) => (
            info.id.clone(),
            provider.or(Some(info.provider.to_string())),
        ),
        None => (model, provider),
    }
}

async fn run_github_token_check(
    checks: &mut Vec<CheckResult>,
    prepared: &PreparedManifest,
    resolved_run: &RunNamespace,
    github_app: Option<fabro_github::GitHubCredentials>,
) {
    if !resolved_run.integrations.github.is_token_requested() {
        return;
    }

    // Resolve InterpString permission values eagerly for token minting and
    // for display in the preflight report.
    let github_permissions = resolved_run
        .integrations
        .github
        .resolve_permissions(process_env_var);

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
            details:     perm_details,
            remediation: Some("No GitHub credentials or origin URL available".to_string()),
        }),
    }
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
    use fabro_model::ProviderId;
    use fabro_model::catalog::LlmCatalogSettings;

    use super::*;

    fn minimal_manifest() -> types::RunManifest {
        types::RunManifest {
            args:      None,
            configs:   Vec::new(),
            cwd:       "/tmp/project".to_string(),
            git:       None,
            goal:      None,
            parent_id: None,
            run_id:    None,
            title:     None,
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
            manifest,
        )
    }

    fn test_catalog() -> Arc<Catalog> {
        Arc::new(Catalog::from_builtin().unwrap())
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
        provider: SandboxProviderKind,
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
            &manifest,
        )
        .unwrap();
        let validated = validate_prepared_manifest(&prepared, test_catalog()).unwrap();
        let resolved = materialize_run(
            prepared.settings.clone(),
            validated.graph(),
            Catalog::builtin(),
            &[ProviderId::anthropic()],
        )
        .run;

        (prepared, resolved)
    }

    #[test]
    fn runtime_daytona_config_preserves_volume_mounts() {
        let settings = fabro_types::settings::run::RunEnvironmentSettings::from_environment(
            "cloud".to_string(),
            fabro_types::settings::run::EnvironmentSettings {
                volumes: vec![fabro_types::settings::run::EnvironmentVolumeSettings {
                    id:         "vol_auth".to_string(),
                    mount_path: "/home/daytona/.config".to_string(),
                    subpath:    Some("agents".to_string()),
                }],
                ..fabro_types::settings::run::EnvironmentSettings::default()
            },
        );

        let config = daytona_config_from_environment(&settings, false);

        assert_eq!(config.volumes.len(), 1);
        assert_eq!(config.volumes[0].volume_id, "vol_auth");
        assert_eq!(config.volumes[0].mount_path, "/home/daytona/.config");
        assert_eq!(config.volumes[0].subpath.as_deref(), Some("agents"));
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
            EnvironmentProvider::Local
        );
    }

    #[tokio::test]
    async fn repository_access_check_skips_when_clone_is_disabled() {
        let (prepared, resolved) = prepared_and_resolved_for_sandbox(
            SandboxProviderKind::Docker,
            false,
            Some(git_context("https://github.com/acme/widgets", "main")),
        );
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_check = Arc::clone(&calls);
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            SandboxProviderKind::Docker,
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
            SandboxProviderKind::Docker,
            true,
            Some(git_context("https://gitlab.com/acme/widgets", "main")),
        );
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_check = Arc::clone(&calls);
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            SandboxProviderKind::Docker,
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
            SandboxProviderKind::Docker,
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
            SandboxProviderKind::Docker,
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
            SandboxProviderKind::Docker,
            true,
            Some(git_context("https://github.com/acme/widgets", "missing")),
        );
        let mut checks = Vec::new();

        let ok = run_repository_access_check_with(
            &mut checks,
            SandboxProviderKind::Docker,
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
            SandboxProviderKind::Docker,
            true,
            Some(git_context("https://github.com/acme/widgets", "main")),
        );

        let spec = preflight_sandbox_spec(
            SandboxProviderKind::Docker,
            &prepared,
            &resolved,
            None,
            None,
        );

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
            preserve_sandbox: None,
            provider:         None,
            environment:      None,
            docker_image:     None,
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
            docker_image:     None,
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

        // v2 merge matrix: run.prepare.steps replaces the whole list across
        // layers, so the higher-precedence workflow layer wins over cli.
        assert_eq!(prepared.settings.run.prepare.commands, vec![
            "workflow-setup".to_string()
        ]);
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
        manifest.target.identifier = "release-flow".to_string();
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
        let validated = validate_prepared_manifest(&prepared, test_catalog()).unwrap();

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
        let validated = validate_prepared_manifest(&prepared, test_catalog()).unwrap();
        assert!(!validated.has_errors());

        let (response, _ok) = run_preflight(state.as_ref(), &prepared, &validated)
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
        let validated = validate_prepared_manifest(&prepared, test_catalog()).unwrap();

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
        let validated = validate_prepared_manifest(&prepared, test_catalog()).unwrap();

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
        let state = crate::test_support::TestAppStateBuilder::new()
            .runtime_settings(
                crate::test_support::default_test_server_settings(),
                RunLayer::default(),
            )
            .max_concurrent_runs(5)
            .provider_base_url("openai", server.url("/v1"))
            .build();
        state
            .vault
            .write()
            .await
            .set(
                "OPENAI_API_KEY",
                "test-openai-key",
                fabro_vault::SecretType::Token,
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
        let validated = validate_prepared_manifest(&prepared, test_catalog()).unwrap();

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

    #[tokio::test]
    async fn preflight_unknown_llm_provider_reports_not_configured() {
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
        let validated = validate_prepared_manifest(&prepared, test_catalog()).unwrap();

        let (response, ok) = run_preflight(state.as_ref(), &prepared, &validated)
            .await
            .unwrap();

        assert!(!ok);
        let llm_check = response.checks.sections[0]
            .checks
            .iter()
            .find(|check| check.name == "LLM" && check.summary == "missing-model")
            .expect("preflight should include the requested custom LLM provider");
        assert_eq!(llm_check.status, types::PreflightCheckResultStatus::Warning);
        assert_eq!(
            llm_check.remediation.as_deref(),
            Some("Provider \"missing-provider\" is not configured")
        );
        assert!(
            llm_check
                .details
                .iter()
                .any(|detail| detail.text == "Provider: missing-provider")
        );
    }

    #[tokio::test]
    async fn preflight_resolves_model_aliases_from_app_state_catalog() {
        let llm_catalog_settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true
aliases = ["vl"]

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
        )
        .expect("catalog fixture should parse");
        let state = crate::test_support::TestAppStateBuilder::new()
            .llm_catalog_settings(llm_catalog_settings)
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
        let validated = validate_prepared_manifest(&prepared, test_catalog()).unwrap();

        let (response, ok) = run_preflight(state.as_ref(), &prepared, &validated)
            .await
            .unwrap();

        assert!(!ok);
        let llm_check = response.checks.sections[0]
            .checks
            .iter()
            .find(|check| check.name == "LLM" && check.summary == "acme-large")
            .expect("preflight should resolve the catalog alias");
        assert_eq!(llm_check.status, types::PreflightCheckResultStatus::Warning);
        assert_eq!(
            llm_check.remediation.as_deref(),
            Some("Provider \"acme\" is not configured")
        );
        assert!(
            llm_check
                .details
                .iter()
                .any(|detail| detail.text == "Provider: acme")
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
}
