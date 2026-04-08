use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use fabro_api::types;
use fabro_config::ConfigLayer;
use fabro_config::effective_settings;
use fabro_config::effective_settings::{EffectiveSettingsLayers, EffectiveSettingsMode};
use fabro_config::project::resolve_working_directory;
use fabro_config::run::{LlmConfig, parse_run_config};
use fabro_config::sandbox::{DockerfileSource, SandboxConfig};
use fabro_graphviz::graph::{Graph, is_llm_handler_type};
use fabro_graphviz::render::apply_direction;
use fabro_llm::Provider;
use fabro_model::Catalog;
use fabro_sandbox::daytona::DaytonaConfig;
use fabro_sandbox::{DockerSandboxOptions, Sandbox, SandboxProvider, SandboxSpec};
use fabro_types::{RunId, Settings};
use fabro_util::check_report::{CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus};
use fabro_validate::Severity;
use fabro_workflow::error::FabroError;
use fabro_workflow::operations::{CreateRunInput, ValidateInput, WorkflowInput, validate};
use fabro_workflow::pipeline::Validated;
use fabro_workflow::workflow_bundle::{BundledWorkflow, WorkflowBundle};

use crate::server::AppState;

#[derive(Clone)]
pub(crate) struct PreparedManifest {
    pub cwd: PathBuf,
    pub git: Option<types::ManifestGit>,
    pub root_source: String,
    pub run_id: Option<RunId>,
    pub settings: Settings,
    pub target_path: PathBuf,
    pub workflow_bundle: WorkflowBundle,
    pub workflow_input: BundledWorkflow,
    pub working_directory: PathBuf,
}

pub(crate) fn prepare_manifest_with_mode(
    server_settings: &Settings,
    manifest: &types::RunManifest,
    local_daemon_mode: bool,
) -> Result<PreparedManifest> {
    if manifest.version != 1 {
        bail!("unsupported manifest version {}", manifest.version);
    }

    let cwd = PathBuf::from(&manifest.cwd);
    let target_path = PathBuf::from(&manifest.target.path);
    let workflow_bundle = workflow_bundle_from_manifest(&manifest.workflows)?;
    let workflow_input = workflow_bundle
        .workflow(&target_path)
        .cloned()
        .ok_or_else(|| anyhow!("manifest target path is missing from workflows map"))?;
    let root_source = workflow_input.source.clone();

    let args_layer = manifest_args_layer(manifest.args.as_ref());
    let workflow_layer = root_workflow_config_layer(manifest, &workflow_input)?;
    let project_layer = manifest
        .configs
        .iter()
        .filter(|config| config.type_ == types::ManifestConfigType::Project)
        .try_fold(ConfigLayer::default(), |layer, config| {
            Ok::<_, anyhow::Error>(parse_manifest_config(config)?.combine(layer))
        })?;
    let user_layer = manifest
        .configs
        .iter()
        .filter(|config| config.type_ == types::ManifestConfigType::User)
        .try_fold(ConfigLayer::default(), |layer, config| {
            Ok::<_, anyhow::Error>(parse_manifest_config(config)?.combine(layer))
        })?;
    let mut settings = effective_settings::resolve_settings(
        EffectiveSettingsLayers::new(args_layer, workflow_layer, project_layer, user_layer),
        Some(server_settings),
        if local_daemon_mode {
            EffectiveSettingsMode::LocalDaemon
        } else {
            EffectiveSettingsMode::RemoteServer
        },
    )?;
    if let Some(goal) = manifest.goal.as_ref() {
        settings.goal = Some(goal.text.clone());
        settings.goal_file = None;
    }

    Ok(PreparedManifest {
        cwd: cwd.clone(),
        git: manifest.git.clone(),
        root_source,
        run_id: manifest
            .run_id
            .as_deref()
            .map(str::parse::<RunId>)
            .transpose()
            .map_err(|err| anyhow!("invalid run ID: {err}"))?,
        settings: settings.clone(),
        target_path,
        workflow_bundle,
        workflow_input,
        working_directory: resolve_working_directory(&settings, &cwd),
    })
}

pub(crate) fn validate_prepared_manifest(
    prepared: &PreparedManifest,
) -> Result<Validated, FabroError> {
    validate(ValidateInput {
        workflow: WorkflowInput::Bundled(prepared.workflow_input.clone()),
        settings: prepared.settings.clone(),
        cwd: prepared.cwd.clone(),
        custom_transforms: Vec::new(),
    })
}

pub(crate) fn create_run_input(prepared: PreparedManifest) -> CreateRunInput {
    CreateRunInput {
        workflow: WorkflowInput::Bundled(prepared.workflow_input),
        settings: prepared.settings,
        cwd: prepared.cwd,
        workflow_slug: None,
        workflow_path: Some(prepared.target_path),
        workflow_bundle: Some(prepared.workflow_bundle),
        submitted_manifest_bytes: None,
        run_id: prepared.run_id,
        host_repo_path: Some(prepared.working_directory.display().to_string()),
        repo_origin_url: prepared
            .git
            .as_ref()
            .map(|git| fabro_github::normalize_repo_origin_url(&git.origin_url)),
        base_branch: prepared.git.as_ref().map(|git| git.branch.clone()),
        provenance: None,
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
        preflight_response(validated, &prepared.target_path, &report, preflight_ok),
        preflight_ok,
    ))
}

pub(crate) fn graph_source(prepared: &PreparedManifest, direction: Option<&str>) -> String {
    direction.map_or_else(
        || prepared.root_source.clone(),
        |direction| apply_direction(&prepared.root_source, direction).into_owned(),
    )
}

fn workflow_bundle_from_manifest(
    workflows: &HashMap<String, types::ManifestWorkflow>,
) -> Result<WorkflowBundle> {
    let workflows = workflows
        .iter()
        .map(|(path, workflow)| {
            let files = workflow
                .files
                .iter()
                .map(|(key, entry)| (PathBuf::from(key), entry.content.clone()))
                .collect::<HashMap<_, _>>();
            Ok::<_, anyhow::Error>((
                PathBuf::from(path),
                BundledWorkflow {
                    logical_path: PathBuf::from(path),
                    source: workflow.source.clone(),
                    files,
                },
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    Ok(WorkflowBundle::new(workflows))
}

fn root_workflow_config_layer(
    manifest: &types::RunManifest,
    workflow: &BundledWorkflow,
) -> Result<ConfigLayer> {
    let Some(root) = manifest.workflows.get(&manifest.target.path) else {
        bail!("manifest target path is missing from workflows map");
    };
    let Some(config) = root.config.as_ref() else {
        return Ok(ConfigLayer::default());
    };

    let mut layer = parse_run_config(&config.source)?;
    resolve_manifest_dockerfile(&mut layer, Path::new(&config.path), &workflow.files)?;
    Ok(layer)
}

fn parse_manifest_config(config: &types::ManifestConfig) -> Result<ConfigLayer> {
    let Some(source) = config.source.as_deref() else {
        return Ok(ConfigLayer::default());
    };
    toml::from_str(source).map_err(Into::into)
}

fn manifest_args_layer(args: Option<&types::ManifestArgs>) -> ConfigLayer {
    let Some(args) = args else {
        return ConfigLayer::default();
    };

    let llm = (args.model.is_some() || args.provider.is_some()).then(|| LlmConfig {
        model: args.model.clone(),
        provider: args.provider.clone(),
        fallbacks: None,
    });
    let sandbox =
        (args.sandbox.is_some() || args.preserve_sandbox.is_some()).then(|| SandboxConfig {
            provider: args.sandbox.clone(),
            preserve: args.preserve_sandbox,
            ..Default::default()
        });

    ConfigLayer {
        llm,
        sandbox,
        verbose: args.verbose,
        dry_run: args.dry_run,
        auto_approve: args.auto_approve,
        no_retro: args.no_retro,
        labels: parse_labels(&args.label),
        ..Default::default()
    }
}

fn parse_labels(labels: &[String]) -> HashMap<String, String> {
    labels
        .iter()
        .filter_map(|label| label.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn resolve_manifest_dockerfile(
    layer: &mut ConfigLayer,
    config_path: &Path,
    files: &HashMap<PathBuf, String>,
) -> Result<()> {
    let source = layer
        .sandbox
        .as_mut()
        .and_then(|sandbox| sandbox.daytona.as_mut())
        .and_then(|daytona| daytona.snapshot.as_mut())
        .and_then(|snapshot| snapshot.dockerfile.as_mut());
    let Some(DockerfileSource::Path { path }) = source else {
        return Ok(());
    };
    let logical_path =
        normalize_logical_path(config_path.parent().unwrap_or_else(|| Path::new(".")), path)
            .ok_or_else(|| anyhow!("unsupported dockerfile reference: {path}"))?;
    let content = files
        .get(&logical_path)
        .cloned()
        .ok_or_else(|| anyhow!("missing bundled dockerfile: {}", logical_path.display()))?;
    *source.unwrap() = DockerfileSource::Inline(content);
    Ok(())
}

fn normalize_logical_path(current_dir: &Path, reference: &str) -> Option<PathBuf> {
    let path = Path::new(reference);
    if path.is_absolute() || reference.starts_with('~') {
        return None;
    }

    let mut normalized = PathBuf::new();
    for component in current_dir.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

async fn build_preflight_report(
    state: &AppState,
    prepared: &PreparedManifest,
    validated: &Validated,
) -> Result<(CheckReport, bool)> {
    let graph = validated.graph();
    let settings = &prepared.settings;
    let sandbox_provider = resolve_sandbox_provider(settings)?;
    let github_app = state
        .github_app_credentials(settings.app_id())
        .await
        .map_err(|err| anyhow!(err))?;
    let mut checks = Vec::new();

    let setup_command_count = settings.setup_commands().len();
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
    checks.push(CheckResult {
        name: "Repository".into(),
        status: CheckStatus::Pass,
        summary: repo_summary,
        details: vec![
            CheckDetail::new(format!("Setup commands: {setup_command_count}")),
            CheckDetail {
                text: format!(
                    "Git: {}",
                    prepared.git.as_ref().map_or("unknown", |git| if git.clean {
                        "clean"
                    } else {
                        "dirty"
                    })
                ),
                warn: prepared.git.as_ref().is_some_and(|git| !git.clean),
            },
        ],
        remediation: None,
    });
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

    let sandbox_ok =
        run_sandbox_check(&mut checks, sandbox_provider, prepared, github_app.clone()).await;
    let llm_ok = run_llm_check(state, &mut checks, graph, settings).await;
    run_github_token_check(&mut checks, prepared, settings, github_app).await;

    let checks_ok = sandbox_ok && llm_ok;

    Ok((
        CheckReport {
            title: "Run Preflight".into(),
            sections: vec![CheckSection {
                title: String::new(),
                checks,
            }],
        },
        checks_ok,
    ))
}

fn resolve_sandbox_provider(settings: &Settings) -> Result<SandboxProvider> {
    Ok(settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.provider.as_deref())
        .map(str::parse::<SandboxProvider>)
        .transpose()
        .map_err(|err| anyhow!("Invalid sandbox provider: {err}"))?
        .unwrap_or_default())
}

fn resolve_daytona_config(settings: &Settings) -> Option<DaytonaConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.daytona.clone())
}

async fn run_sandbox_check(
    checks: &mut Vec<CheckResult>,
    sandbox_provider: SandboxProvider,
    prepared: &PreparedManifest,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> bool {
    let daytona_config = resolve_daytona_config(&prepared.settings);
    let sandbox_result: Result<Arc<dyn Sandbox>, String> = match sandbox_provider {
        SandboxProvider::Local => SandboxSpec::Local {
            working_directory: prepared.working_directory.clone(),
        }
        .build(None)
        .await
        .map_err(|err| err.to_string()),
        SandboxProvider::Docker => SandboxSpec::Docker {
            config: DockerSandboxOptions {
                host_working_directory: prepared.working_directory.to_string_lossy().to_string(),
                ..DockerSandboxOptions::default()
            },
        }
        .build(None)
        .await
        .map_err(|err| err.to_string()),
        SandboxProvider::Daytona => SandboxSpec::Daytona {
            config: daytona_config.unwrap_or_default(),
            github_app,
            run_id: None,
            clone_branch: prepared.git.as_ref().map(|git| git.branch.clone()),
        }
        .build(None)
        .await
        .map_err(|err| format!("Daytona sandbox creation failed: {err}")),
    };

    match sandbox_result {
        Ok(sandbox) => match sandbox.initialize().await {
            Ok(()) => {
                let _ = sandbox.cleanup().await;
                checks.push(CheckResult {
                    name: "Sandbox".into(),
                    status: CheckStatus::Pass,
                    summary: sandbox_provider.to_string(),
                    details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                    remediation: None,
                });
                true
            }
            Err(err) => {
                let _ = sandbox.cleanup().await;
                checks.push(CheckResult {
                    name: "Sandbox".into(),
                    status: CheckStatus::Error,
                    summary: "failed".into(),
                    details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                    remediation: Some(format!("Sandbox init failed: {err}")),
                });
                false
            }
        },
        Err(err) => {
            checks.push(CheckResult {
                name: "Sandbox".into(),
                status: CheckStatus::Error,
                summary: "failed".into(),
                details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
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
    settings: &Settings,
) -> bool {
    let (model, provider) = resolve_model_provider(settings, graph);
    let default_provider = provider.as_deref().unwrap_or("anthropic");

    match state.build_llm_client().await {
        Ok(client) => {
            let configured = client
                .provider_names()
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>();
            let mut model_providers = std::collections::BTreeSet::new();

            for node in graph.nodes.values() {
                if !is_llm_handler_type(node.handler_type()) {
                    continue;
                }
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
                        if !configured.iter().any(|name| name == provider_name) {
                            status = CheckStatus::Warning;
                            all_ok = false;
                        }
                        checks.push(CheckResult {
                            name: "LLM".into(),
                            status,
                            summary: model_id.clone(),
                            details: vec![CheckDetail::new(format!("Provider: {provider_name}"))],
                            remediation: (status == CheckStatus::Warning)
                                .then(|| format!("Provider \"{provider_name}\" is not configured")),
                        });
                    }
                    Err(err) => {
                        checks.push(CheckResult {
                            name: "LLM".into(),
                            status: CheckStatus::Error,
                            summary: model_id.clone(),
                            details: vec![CheckDetail::new(format!("Provider: {provider_name}"))],
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
                name: "LLM".into(),
                status: CheckStatus::Error,
                summary: "initialization failed".into(),
                details: vec![],
                remediation: Some(format!("LLM client init failed: {err}")),
            });
            false
        }
    }
}

fn resolve_model_provider(settings: &Settings, graph: &Graph) -> (String, Option<String>) {
    let configured_model = settings.llm.as_ref().and_then(|llm| llm.model.as_deref());
    let configured_provider = settings
        .llm
        .as_ref()
        .and_then(|llm| llm.provider.as_deref());

    let provider = configured_provider
        .or_else(|| {
            graph
                .attrs
                .get("default_provider")
                .and_then(|value| value.as_str())
        })
        .map(String::from);
    let model = configured_model
        .or_else(|| {
            graph
                .attrs
                .get("default_model")
                .and_then(|value| value.as_str())
        })
        .map_or_else(
            || {
                let catalog = Catalog::builtin();
                let info = provider
                    .as_deref()
                    .and_then(|value| value.parse::<Provider>().ok())
                    .and_then(|provider| catalog.default_for_provider(provider))
                    .unwrap_or_else(|| catalog.default_from_env());
                info.id.clone()
            },
            String::from,
        );

    match Catalog::builtin().get(&model) {
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
    settings: &Settings,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) {
    let Some(github_permissions) = settings.github_permissions() else {
        return;
    };
    if github_permissions.is_empty() {
        return;
    }

    let perm_details = github_permissions
        .iter()
        .map(|(key, value)| CheckDetail::new(format!("{key}: {value}")))
        .collect::<Vec<_>>();
    match (&github_app, prepared.git.as_ref()) {
        (Some(creds), Some(git)) => {
            match mint_github_token(creds, &git.origin_url, github_permissions).await {
                Ok(_) => checks.push(CheckResult {
                    name: "GitHub Token".into(),
                    status: CheckStatus::Pass,
                    summary: "minted".into(),
                    details: perm_details,
                    remediation: None,
                }),
                Err(err) => checks.push(CheckResult {
                    name: "GitHub Token".into(),
                    status: CheckStatus::Error,
                    summary: "failed".into(),
                    details: perm_details,
                    remediation: Some(format!("Failed to mint GitHub token: {err}")),
                }),
            }
        }
        _ => checks.push(CheckResult {
            name: "GitHub Token".into(),
            status: CheckStatus::Warning,
            summary: "skipped".into(),
            details: vec![],
            remediation: Some("No GitHub App credentials or origin URL available".to_string()),
        }),
    }
}

async fn mint_github_token(
    creds: &fabro_github::GitHubAppCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> Result<String> {
    let https_url = fabro_github::ssh_url_to_https(origin_url);
    let (owner, repo) =
        fabro_github::parse_github_owner_repo(&https_url).map_err(|err| anyhow!("{err}"))?;
    let jwt = fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem)
        .map_err(|err| anyhow!("{err}"))?;
    let client = reqwest::Client::new();
    let perms_json = serde_json::to_value(permissions)?;
    fabro_github::create_installation_access_token_with_permissions(
        &client,
        &jwt,
        &owner,
        &repo,
        &fabro_github::github_api_base_url(),
        perms_json,
    )
    .await
    .map_err(|err| anyhow!("{err}"))
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
        workflow: types::PreflightWorkflowSummary {
            diagnostics: diagnostics_to_api(validated.diagnostics()),
            edges: i64::try_from(validated.graph().edges.len()).unwrap(),
            goal: validated.graph().goal().to_string(),
            graph_path: Some(target_path.display().to_string()),
            name: validated.graph().name.clone(),
            nodes: i64::try_from(validated.graph().nodes.len()).unwrap(),
        },
    }
}

fn diagnostics_to_api(
    diagnostics: &[fabro_validate::Diagnostic],
) -> Vec<types::WorkflowDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| types::WorkflowDiagnostic {
            edge: diagnostic
                .edge
                .as_ref()
                .map(|edge: &(String, String)| [edge.0.clone(), edge.1.clone()]),
            fix: diagnostic.fix.clone(),
            message: diagnostic.message.clone(),
            node_id: diagnostic.node_id.clone(),
            rule: diagnostic.rule.clone(),
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
                        details: check
                            .details
                            .iter()
                            .map(|detail| types::PreflightCheckDetail {
                                text: detail.text.clone(),
                                warn: detail.warn,
                            })
                            .collect(),
                        name: check.name.clone(),
                        remediation: check.remediation.clone(),
                        status: match check.status {
                            CheckStatus::Pass => types::PreflightCheckResultStatus::Pass,
                            CheckStatus::Warning => types::PreflightCheckResultStatus::Warning,
                            CheckStatus::Error => types::PreflightCheckResultStatus::Error,
                        },
                        summary: check.summary.clone(),
                    })
                    .collect(),
                title: section.title.clone(),
            })
            .collect(),
        title: report.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_manifest() -> types::RunManifest {
        types::RunManifest {
            args: None,
            configs: Vec::new(),
            cwd: "/tmp/project".to_string(),
            git: None,
            goal: None,
            run_id: None,
            target: types::ManifestTarget {
                identifier: "workflow.fabro".to_string(),
                path: "workflow.fabro".to_string(),
            },
            version: 1,
            workflows: HashMap::from([(
                "workflow.fabro".to_string(),
                types::ManifestWorkflow {
                    config: None,
                    files: HashMap::new(),
                    source:
                        "digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }"
                            .to_string(),
                },
            )]),
        }
    }

    #[test]
    fn prepare_manifest_does_not_inherit_server_dry_run_fallback() {
        let server_settings = Settings {
            dry_run: Some(true),
            storage_dir: Some(PathBuf::from("/srv/fabro")),
            ..Default::default()
        };

        let prepared =
            prepare_manifest_with_mode(&server_settings, &minimal_manifest(), false).unwrap();

        assert_eq!(prepared.settings.dry_run, None);
        assert_eq!(
            prepared.settings.storage_dir,
            Some(PathBuf::from("/srv/fabro"))
        );
    }

    #[test]
    fn prepare_manifest_preserves_explicit_manifest_dry_run() {
        let server_settings = Settings {
            dry_run: Some(true),
            storage_dir: Some(PathBuf::from("/srv/fabro")),
            ..Default::default()
        };
        let mut manifest = minimal_manifest();
        manifest.args = Some(types::ManifestArgs {
            auto_approve: None,
            dry_run: Some(true),
            label: Vec::new(),
            model: None,
            no_retro: None,
            preserve_sandbox: None,
            provider: None,
            sandbox: None,
            verbose: None,
        });

        let prepared = prepare_manifest_with_mode(&server_settings, &manifest, false).unwrap();

        assert_eq!(prepared.settings.dry_run, Some(true));
    }

    #[test]
    fn prepare_manifest_local_daemon_prefers_bundled_settings_without_duplication() {
        let server_settings: Settings = toml::from_str(
            r#"
storage_dir = "/srv/fabro"

[setup]
commands = ["cli-setup"]

[git]
app_id = "snapshotted-app-id"
"#,
        )
        .unwrap();

        let mut manifest = minimal_manifest();
        manifest.workflows.get_mut("workflow.fabro").unwrap().config =
            Some(types::ManifestWorkflowConfig {
                path: "workflow.toml".to_string(),
                source: r#"
version = 1

[setup]
commands = ["workflow-setup"]
"#
                .to_string(),
            });
        manifest.configs.push(types::ManifestConfig {
            path: Some("/tmp/home/.fabro/settings.toml".to_string()),
            source: Some(
                r#"
[setup]
commands = ["cli-setup"]

[git]
app_id = "snapshotted-app-id"
"#
                .to_string(),
            ),
            type_: types::ManifestConfigType::User,
        });

        let prepared = prepare_manifest_with_mode(&server_settings, &manifest, true).unwrap();

        assert_eq!(
            prepared
                .settings
                .setup
                .as_ref()
                .map(|setup| setup.commands.clone()),
            Some(vec!["workflow-setup".to_string(), "cli-setup".to_string(),])
        );
        assert_eq!(prepared.settings.app_id(), Some("snapshotted-app-id"));
        assert_eq!(
            prepared.settings.storage_dir,
            Some(PathBuf::from("/srv/fabro"))
        );
    }
}
