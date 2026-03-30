use std::path::Path;
use std::sync::Arc;

use anyhow::bail;
use fabro_config::project::{resolve_workflow_path, resolve_working_directory};
use fabro_config::{ConfigLayer, FabroSettings};
use fabro_graphviz::graph::{Graph, is_llm_handler_type};
use fabro_llm::client::Client as LlmClient;
use fabro_model::{Catalog, Provider};
use fabro_sandbox::daytona::{DaytonaConfig, detect_repo_info};
use fabro_sandbox::ssh::SshConfig;
use fabro_sandbox::{
    DockerSandboxConfig, Sandbox, SandboxProvider, SandboxSpec, detect_clone_params,
};
use fabro_util::terminal::Styles;
use fabro_workflows::git::{GitSyncStatus, sync_status};
use fabro_workflows::operations::{ValidateInput, WorkflowInput, validate};

use crate::args::{GlobalArgs, PreflightArgs};
use crate::cli_config::{cli_layer_with_globals, load_cli_settings_with_globals};
use crate::shared::github::build_github_app_credentials;

pub(crate) async fn execute(mut args: PreflightArgs, globals: &GlobalArgs) -> anyhow::Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let cli = cli_layer_with_globals(globals)?;
    let cli_settings: FabroSettings = load_cli_settings_with_globals(globals)?;
    args.verbose = args.verbose || cli_settings.verbose_enabled();

    let github_app = build_github_app_credentials(cli_settings.app_id());
    let cli_args_config = ConfigLayer::try_from(&args)?;
    let cwd = std::env::current_dir()?;
    let settings = cli_args_config
        .combine(ConfigLayer::for_workflow(&args.workflow, &cwd)?)
        .combine(cli)
        .resolve()?;
    let resolution = resolve_workflow_path(&args.workflow, &cwd)?;
    let working_directory = resolve_working_directory(&settings, &cwd);

    let (origin_url, detected_base_branch) = detect_repo_info(&working_directory)
        .map(|(url, branch)| (Some(url), branch))
        .unwrap_or((None, None));
    let git_status = sync_status(
        &working_directory,
        "origin",
        detected_base_branch.as_deref(),
    );

    let sandbox_provider = resolve_sandbox_provider(args.sandbox.map(Into::into), &settings)?;

    let validated = validate(ValidateInput {
        workflow: WorkflowInput::Path(args.workflow.clone()),
        settings: settings.clone(),
        cwd,
        custom_transforms: Vec::new(),
    })?;
    super::run::output::print_workflow_report(&validated, Some(&resolution.dot_path), styles);
    if validated.has_errors() {
        bail!("Validation failed");
    }

    run_preflight(
        validated.graph(),
        &settings,
        args.model.as_deref(),
        args.provider.as_deref(),
        git_status,
        sandbox_provider,
        &working_directory,
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
    graph: &Graph,
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
        .map_or_else(
            || {
                let catalog = Catalog::builtin();
                let info = provider
                    .as_deref()
                    .and_then(|s| s.parse::<Provider>().ok())
                    .and_then(|p| catalog.default_for_provider(p))
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

fn parse_sandbox_provider(settings: &FabroSettings) -> anyhow::Result<Option<SandboxProvider>> {
    settings
        .sandbox_settings()
        .and_then(|s| s.provider.as_deref())
        .map(str::parse::<SandboxProvider>)
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

fn resolve_daytona_config(settings: &FabroSettings) -> Option<DaytonaConfig> {
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

fn resolve_ssh_config(settings: &FabroSettings) -> Option<SshConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.ssh.clone())
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
    graph: &Graph,
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
    let repo_summary = origin_url.map_or_else(
        || "unknown".into(),
        |url| {
            let https = fabro_github::ssh_url_to_https(url);
            fabro_github::parse_github_owner_repo(&https).map_or_else(
                |_| url.to_string(),
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
        SandboxProvider::Local => SandboxSpec::Local {
            working_directory: working_directory.to_path_buf(),
        }
        .build(None)
        .await
        .map_err(|e| e.to_string()),
        SandboxProvider::Docker => SandboxSpec::Docker {
            config: DockerSandboxConfig {
                host_working_directory: working_directory.to_string_lossy().to_string(),
                ..DockerSandboxConfig::default()
            },
        }
        .build(None)
        .await
        .map_err(|e| e.to_string()),
        SandboxProvider::Daytona => SandboxSpec::Daytona {
            config: daytona_config.unwrap_or_default(),
            github_app: github_app.clone(),
            run_id: None,
            clone_branch: None,
        }
        .build(None)
        .await
        .map_err(|e| format!("Daytona sandbox creation failed: {e}")),
        #[cfg(feature = "exedev")]
        SandboxProvider::Exe => SandboxSpec::Exe {
            config: exe_config.unwrap_or_default(),
            clone_params: detect_clone_params(working_directory),
            run_id: None,
            github_app: None,
            mgmt_destination: "exe.dev".to_string(),
        }
        .build(None)
        .await
        .map_err(|e| format!("exe sandbox creation failed: {e}")),
        #[cfg(not(feature = "exedev"))]
        SandboxProvider::Exe => Err("exe sandbox requires the exedev feature".to_string()),
        SandboxProvider::Ssh => match ssh_config {
            Some(config) => SandboxSpec::Ssh {
                config,
                clone_params: detect_clone_params(working_directory),
                run_id: None,
                github_app: None,
            }
            .build(None)
            .await
            .map_err(|e| e.to_string()),
            None => Err("SSH sandbox requires [sandbox.ssh] config".to_string()),
        },
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
    let llm_ok = match LlmClient::from_env().await {
        Ok(c) => {
            let configured: Vec<String> = c
                .provider_names()
                .iter()
                .map(std::string::ToString::to_string)
                .collect();

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
