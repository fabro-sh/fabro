use std::path::Path;
use std::sync::Arc;

use anyhow::bail;
use fabro_agent::{DockerSandbox, DockerSandboxConfig, LocalSandbox, Sandbox};
use fabro_config::{FabroConfig, FabroSettings};
use fabro_model::{Catalog, Provider};
use fabro_sandbox::SandboxProvider;
use fabro_util::terminal::Styles;
use fabro_workflows::git::GitSyncStatus;

use crate::args::PreflightArgs;

pub async fn execute(mut args: PreflightArgs) -> anyhow::Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let cli_defaults = fabro_config::cli::load_cli_config(None)?;
    let cli_config: FabroSettings = cli_defaults.clone().try_into()?;
    args.verbose = args.verbose || cli_config.verbose_enabled();

    let github_app = crate::shared::github::build_github_app_credentials(cli_config.app_id());
    let cli_args_config = FabroConfig::try_from(&args)?;
    let settings = fabro_workflows::operations::resolve_settings_for_path(
        &args.workflow,
        cli_defaults,
        cli_args_config,
        true,
    )?;
    let resolved = fabro_workflows::operations::resolve_workflow(
        fabro_workflows::operations::ResolveWorkflowRequest {
            workflow: fabro_workflows::operations::WorkflowInput::Path(args.workflow.clone()),
            settings: settings.clone(),
            cwd: std::env::current_dir()?,
        },
    )?;

    let (origin_url, detected_base_branch) =
        fabro_sandbox::daytona::detect_repo_info(&resolved.working_directory)
            .map(|(url, branch)| (Some(url), branch))
            .unwrap_or((None, None));
    let git_status = fabro_workflows::git::sync_status(
        &resolved.working_directory,
        "origin",
        detected_base_branch.as_deref(),
    );

    let sandbox_provider = resolve_sandbox_provider(args.sandbox.map(Into::into), &settings)?;

    let validated = fabro_workflows::operations::validate(
        &resolved.raw_source,
        fabro_workflows::operations::ValidateOptions {
            base_dir: resolved.base_dir.clone(),
            settings: Some(resolved.settings.clone()),
            goal_override: resolved.goal_override.clone(),
            ..Default::default()
        },
    )?;
    super::run::output::print_workflow_report(&validated, resolved.dot_path.as_deref(), styles);
    if validated.has_errors() {
        bail!("Validation failed");
    }

    run_preflight(
        validated.graph(),
        &resolved.settings,
        args.model.as_deref(),
        args.provider.as_deref(),
        git_status,
        sandbox_provider,
        &resolved.working_directory,
        styles,
        github_app,
        origin_url.as_deref(),
    )
    .await
}

fn resolve_model_provider(
    cli_model: Option<&str>,
    cli_provider: Option<&str>,
    settings: &FabroSettings,
    graph: &fabro_graphviz::graph::Graph,
) -> (String, Option<String>) {
    let configured_model = settings.llm.as_ref().and_then(|llm| llm.model.as_deref());
    let configured_provider = settings
        .llm
        .as_ref()
        .and_then(|llm| llm.provider.as_deref());

    let provider = cli_provider
        .or(configured_provider)
        .or_else(|| graph.attrs.get("default_provider").and_then(|v| v.as_str()))
        .map(String::from);

    let model = cli_model
        .or(configured_model)
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

    match Catalog::builtin().get(&model) {
        Some(info) => (
            info.id.clone(),
            provider.or(Some(info.provider.to_string())),
        ),
        None => (model, provider),
    }
}

fn parse_sandbox_provider(settings: &FabroSettings) -> anyhow::Result<Option<SandboxProvider>> {
    settings
        .sandbox_settings()
        .and_then(|s| s.provider.as_deref())
        .map(|s| s.parse::<SandboxProvider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("Invalid sandbox provider: {e}"))
}

fn resolve_sandbox_provider(
    cli: Option<SandboxProvider>,
    settings: &FabroSettings,
) -> anyhow::Result<SandboxProvider> {
    Ok(cli
        .or(parse_sandbox_provider(settings)?)
        .unwrap_or_default())
}

fn resolve_daytona_config(
    settings: &FabroSettings,
) -> Option<fabro_sandbox::daytona::DaytonaConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.daytona.clone())
}

#[cfg(feature = "exedev")]
fn resolve_exe_config(settings: &FabroSettings) -> Option<fabro_sandbox::exe::ExeConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.exe.clone())
}

#[cfg(feature = "exedev")]
fn resolve_exe_clone_params(cwd: &Path) -> Option<fabro_sandbox::exe::GitCloneParams> {
    let (detected_url, branch) = match fabro_sandbox::daytona::detect_repo_info(cwd) {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!("No git repo detected for exe.dev clone: {err}");
            return None;
        }
    };
    let url = fabro_github::ssh_url_to_https(&detected_url);
    Some(fabro_sandbox::exe::GitCloneParams { url, branch })
}

fn resolve_ssh_config(settings: &FabroSettings) -> Option<fabro_sandbox::ssh::SshConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.ssh.clone())
}

fn resolve_ssh_clone_params(cwd: &Path) -> Option<fabro_sandbox::ssh::GitCloneParams> {
    let (detected_url, branch) = match fabro_sandbox::daytona::detect_repo_info(cwd) {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!("No git repo detected for SSH clone: {err}");
            return None;
        }
    };
    let url = fabro_github::ssh_url_to_https(&detected_url);
    Some(fabro_sandbox::ssh::GitCloneParams { url, branch })
}

async fn mint_github_token(
    creds: &fabro_github::GitHubAppCredentials,
    origin_url: &str,
    permissions: &std::collections::HashMap<String, String>,
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

#[allow(clippy::too_many_arguments)]
async fn run_preflight(
    graph: &fabro_graphviz::graph::Graph,
    settings: &FabroSettings,
    cli_model: Option<&str>,
    cli_provider: Option<&str>,
    git_status: GitSyncStatus,
    sandbox_provider: SandboxProvider,
    working_directory: &Path,
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
    spinner.set_message("Running preflight checks...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    let mut checks: Vec<CheckResult> = Vec::new();

    let setup_command_count = settings.setup_commands().len();
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

    let (model, provider) = resolve_model_provider(cli_model, cli_provider, settings, graph);
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

    let daytona_config = resolve_daytona_config(settings);
    #[cfg(feature = "exedev")]
    let exe_config = resolve_exe_config(settings);
    let ssh_config = resolve_ssh_config(settings);

    let sandbox_result: Result<Arc<dyn Sandbox>, String> = match sandbox_provider {
        SandboxProvider::Docker => {
            let config = DockerSandboxConfig {
                host_working_directory: working_directory.to_string_lossy().to_string(),
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
                    let clone_params = resolve_exe_clone_params(working_directory);
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
                let clone_params = resolve_ssh_clone_params(working_directory);
                let env = fabro_sandbox::ssh::SshSandbox::new(config, clone_params, None, None);
                Ok(Arc::new(env) as Arc<dyn Sandbox>)
            }
            None => Err("SSH sandbox requires [sandbox.ssh] config".to_string()),
        },
        SandboxProvider::Local => {
            Ok(Arc::new(LocalSandbox::new(working_directory.to_path_buf())) as Arc<dyn Sandbox>)
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

    let default_provider = provider.as_deref().unwrap_or("anthropic");
    let llm_ok = match fabro_llm::client::Client::from_env().await {
        Ok(c) => {
            let configured: Vec<String> =
                c.provider_names().iter().map(|s| s.to_string()).collect();

            let mut model_providers = std::collections::BTreeSet::new();
            for node in graph.nodes.values() {
                if !fabro_graphviz::graph::is_llm_handler_type(node.handler_type()) {
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

    if let Some(github_permissions) = settings.github_permissions() {
        if !github_permissions.is_empty() {
            let perm_details: Vec<CheckDetail> = github_permissions
                .iter()
                .map(|(k, v)| CheckDetail::new(format!("{k}: {v}")))
                .collect();
            match (&github_app, origin_url) {
                (Some(creds), Some(url)) => {
                    match mint_github_token(creds, url, github_permissions).await {
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
