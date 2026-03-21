mod cli_config;
mod commands;
mod doctor;
mod init;
mod install;
mod logging;
mod provider_auth;
mod skill;
mod upgrade;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::debug;

const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("FABRO_GIT_SHA"),
    " ",
    env!("FABRO_BUILD_DATE"),
    ")"
);

#[derive(Parser)]
#[command(name = "fabro", version, long_version = LONG_VERSION)]
struct Cli {
    /// Enable DEBUG-level logging (default is INFO)
    #[arg(long, global = true)]
    debug: bool,

    /// Disable automatic upgrade check
    #[arg(long, global = true)]
    no_upgrade_check: bool,

    /// Execution mode: standalone (in-process) or server (delegate to API)
    #[cfg(feature = "server")]
    #[arg(long, global = true, value_parser = parse_execution_mode)]
    mode: Option<cli_config::ExecutionMode>,

    /// Server URL (overrides server.base_url from cli.toml)
    #[cfg(feature = "server")]
    #[arg(long, global = true)]
    server_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[cfg(feature = "server")]
fn parse_execution_mode(s: &str) -> Result<cli_config::ExecutionMode, String> {
    match s {
        "standalone" => Ok(cli_config::ExecutionMode::Standalone),
        "server" => Ok(cli_config::ExecutionMode::Server),
        _ => Err(format!(
            "invalid mode '{s}', expected 'standalone' or 'server'"
        )),
    }
}

#[derive(Subcommand)]
enum Command {
    /// LLM prompt operations
    #[command(hide = true)]
    Llm {
        #[command(subcommand)]
        command: LlmCommand,
    },
    /// Run an agentic coding session
    #[command(hide = true)]
    Exec(fabro_agent::cli::AgentArgs),
    /// Launch a workflow run
    Run(commands::run::RunArgs),
    /// Create a workflow run (allocate run dir, persist spec)
    Create(commands::run::RunArgs),
    /// Start a created workflow run (spawn engine process)
    Start {
        /// Run ID prefix or workflow name
        run: String,
    },
    /// Attach to a running or finished workflow run
    Attach {
        /// Run ID prefix or workflow name
        run: String,
    },
    /// Internal: run the engine process (reads spec.json from run dir)
    #[command(name = "_run_engine", hide = true)]
    RunEngine {
        /// Path to the run directory
        #[arg(long)]
        run_dir: PathBuf,
    },
    /// Validate a workflow
    Validate(commands::validate::ValidateArgs),
    /// Render a workflow graph as SVG or PNG
    Graph(commands::graph::GraphArgs),
    /// Parse a DOT file and print its AST
    #[command(hide = true)]
    Parse(commands::parse::ParseArgs),
    /// Inspect and copy run assets (screenshots, reports, traces)
    Asset {
        #[command(subcommand)]
        command: AssetCommand,
    },
    /// Copy files to/from a run's sandbox
    Cp(commands::cp::CpArgs),
    /// Get a preview URL for a port on a run's sandbox
    Preview(commands::preview::PreviewArgs),
    /// SSH into a run's Daytona sandbox
    Ssh(commands::ssh::SshArgs),
    /// Show the diff of changes from a workflow run
    #[command(hide = true)]
    Diff(commands::diff::DiffArgs),
    /// View the event log of a workflow run
    Logs(commands::logs::LogsArgs),
    /// Show detailed information about a workflow run
    Inspect(commands::inspect::InspectArgs),
    /// List and test LLM models
    Model {
        #[command(subcommand)]
        command: Option<fabro_llm::cli::ModelsCommand>,
    },
    /// Start the HTTP API server
    #[cfg(feature = "server")]
    Serve(fabro_api::serve::ServeArgs),
    /// Check environment and integration health
    Doctor {
        /// Show detailed information for each check
        #[arg(short, long)]
        verbose: bool,

        /// Skip live service probes (LLM, sandbox, API, web, Brave Search)
        #[arg(long)]
        dry_run: bool,
    },
    /// Initialize a new project (deprecated: use `repo init`)
    #[command(hide = true)]
    Init,
    /// Set up the Fabro environment (LLMs, certs, GitHub)
    Install {
        /// Base URL for the web UI (used for OAuth callback URLs)
        #[arg(long, default_value = "http://localhost:5173")]
        web_url: String,
    },
    /// List workflow runs
    #[command(hide = true)]
    Ps(commands::runs::RunsListArgs),
    /// Remove one or more workflow runs
    Rm(commands::runs::RunsRemoveArgs),
    /// Pull request operations
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Skill management
    #[command(hide = true)]
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// Manage secrets in ~/.fabro/.env
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    /// Resume an interrupted workflow run
    Resume(commands::resume::ResumeArgs),
    /// Rewind a workflow run to an earlier checkpoint
    Rewind(commands::rewind::RewindArgs),
    /// Fork a workflow run from an earlier checkpoint into a new run
    Fork(commands::fork::ForkArgs),
    /// Block until a workflow run completes
    Wait(commands::wait::WaitArgs),
    /// Workflow operations
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Open the Discord community in the browser
    Discord,
    /// Open the docs website in the browser
    Docs,
    /// Upgrade fabro to the latest version
    Upgrade(upgrade::UpgradeArgs),
    /// Repository commands
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Provider operations
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// System maintenance commands
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },
    /// Send a queued analytics event (internal)
    #[command(name = "__send_analytics", hide = true)]
    SendAnalytics {
        /// Path to the JSON event file
        path: PathBuf,
    },
    /// Send a queued panic event to Sentry (internal)
    #[command(name = "__send_panic", hide = true)]
    SendPanic {
        /// Path to the JSON event file
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum PrCommand {
    /// Create a pull request from a completed run
    Create(commands::pr::PrCreateArgs),
    /// List pull requests from workflow runs
    List(commands::pr::PrListArgs),
    /// View pull request details
    View(commands::pr::PrViewArgs),
    /// Merge a pull request
    Merge(commands::pr::PrMergeArgs),
    /// Close a pull request
    Close(commands::pr::PrCloseArgs),
}

#[derive(Subcommand)]
enum SystemCommand {
    /// Delete old workflow runs
    Prune(commands::runs::RunsPruneArgs),
    /// Show disk usage
    Df(commands::runs::DfArgs),
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Initialize a new project
    Init {
        /// Also install the fabro-create-workflow skill
        #[arg(long, hide = true)]
        skill: bool,
    },
    /// Remove fabro.toml and fabro/ directory
    Deinit,
}

#[derive(Subcommand)]
enum SecretCommand {
    /// Get a secret value
    Get(commands::secret::SecretGetArgs),
    /// List secret names
    #[command(alias = "ls")]
    List(commands::secret::SecretListArgs),
    /// Remove a secret
    Rm(commands::secret::SecretRmArgs),
    /// Set a secret value
    Set(commands::secret::SecretSetArgs),
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Install a built-in skill
    Install(skill::SkillInstallArgs),
}

#[derive(Subcommand)]
enum WorkflowCommand {
    /// List available workflows
    List(commands::workflow::WorkflowListArgs),
    /// Create a new workflow
    Create(commands::workflow::WorkflowCreateArgs),
}

#[derive(Subcommand)]
enum ProviderCommand {
    /// Log in to an LLM provider
    Login(commands::provider::ProviderLoginArgs),
}

#[derive(Subcommand)]
enum AssetCommand {
    /// List assets for a workflow run
    List(commands::asset::AssetListArgs),
    /// Copy assets from a workflow run
    Cp(commands::asset::AssetCpArgs),
}

#[derive(Subcommand)]
enum LlmCommand {
    /// Execute a prompt
    Prompt(fabro_llm::cli::PromptArgs),
    /// Interactive multi-turn chat
    Chat(fabro_llm::cli::ChatArgs),
}

pub(crate) fn build_github_app_credentials(
    app_id: Option<&str>,
) -> Option<fabro_github::GitHubAppCredentials> {
    let app_id = app_id?;
    let raw = std::env::var("GITHUB_APP_PRIVATE_KEY").ok()?;
    let private_key_pem = if raw.starts_with("-----") {
        raw
    } else {
        let pem_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &raw).ok()?;
        String::from_utf8(pem_bytes).ok()?
    };
    Some(fabro_github::GitHubAppCredentials {
        app_id: app_id.to_string(),
        private_key_pem,
    })
}

#[tokio::main]
async fn main() {
    fabro_telemetry::panic::install_panic_hook();
    fabro_telemetry::init_cli();

    let start = std::time::Instant::now();
    let raw_args: Vec<String> = std::env::args().collect();

    let (command_name, result) = main_inner().await;
    let duration_ms = start.elapsed().as_millis() as u64;

    let is_error = result.is_err();
    let command = fabro_telemetry::sanitize::sanitize_command(&raw_args, &command_name);
    let repository = fabro_telemetry::git::repository_identifier();
    let ci = std::env::var("CI").is_ok();
    if is_error {
        fabro_telemetry::track!("CLI Errored", {
            "subcommand": command_name,
            "command": command,
            "durationMs": duration_ms,
            "repository": repository,
            "ci": ci,
            "success": false,
            "exitCode": 1,
        }, error);
    } else {
        fabro_telemetry::track!("CLI Executed", {
            "subcommand": command_name,
            "command": command,
            "durationMs": duration_ms,
            "repository": repository,
            "ci": ci,
            "success": true,
            "exitCode": 0,
        });
    }
    fabro_telemetry::shutdown();

    if let Err(err) = result {
        let style = console::Style::new().red().bold();
        for (i, cause) in err.chain().enumerate() {
            let text = cause.to_string();
            if i == 0 {
                for (j, line) in text.lines().enumerate() {
                    if j == 0 {
                        eprintln!("{} {line}", style.apply_to("error:"));
                    } else {
                        eprintln!("  {line}");
                    }
                }
            } else {
                for line in text.lines() {
                    eprintln!("  > {line}");
                }
            }
        }
        std::process::exit(1);
    }
}

async fn main_inner() -> (String, Result<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    if let Some(home) = dirs::home_dir() {
        let env_path = home.join(".fabro").join(".env");
        if dotenvy::from_path(&env_path).is_ok() {
            debug!(path = %env_path.display(), "Loaded environment file");
        }
    }

    let command_name = match &cli.command {
        Command::Llm { command } => match command {
            LlmCommand::Prompt(_) => "llm prompt",
            LlmCommand::Chat(_) => "llm chat",
        },
        Command::Asset { command } => match command {
            AssetCommand::List(_) => "asset list",
            AssetCommand::Cp(_) => "asset cp",
        },
        Command::Exec(_) => "exec",
        Command::Run(_) => "run",
        Command::Create(_) => "create",
        Command::Start { .. } => "start",
        Command::Attach { .. } => "attach",
        Command::RunEngine { .. } => "_run_engine",
        Command::Validate(_) => "validate",
        Command::Graph(_) => "graph",
        Command::Parse(_) => "parse",
        Command::Cp(_) => "cp",
        Command::Preview(_) => "preview",
        Command::Ssh(_) => "ssh",
        Command::Diff(_) => "diff",
        Command::Logs(_) => "logs",
        Command::Inspect(_) => "inspect",
        Command::Model { command } => match command {
            Some(fabro_llm::cli::ModelsCommand::List { .. }) => "model list",
            Some(fabro_llm::cli::ModelsCommand::Test { .. }) => "model test",
            None => "model",
        },
        #[cfg(feature = "server")]
        Command::Serve(_) => "serve",
        Command::Doctor { .. } => "doctor",
        Command::Repo { command } => match command {
            RepoCommand::Init { .. } => "repo init",
            RepoCommand::Deinit => "repo deinit",
        },
        Command::Init => "init",
        Command::Install { .. } => "install",
        Command::Ps(_) => "ps",
        Command::Rm(_) => "rm",
        Command::Pr { command } => match command {
            PrCommand::Create(_) => "pr create",
            PrCommand::List(_) => "pr list",
            PrCommand::View(_) => "pr view",
            PrCommand::Merge(_) => "pr merge",
            PrCommand::Close(_) => "pr close",
        },
        Command::Secret { command } => match command {
            SecretCommand::Get(_) => "secret get",
            SecretCommand::List(_) => "secret list",
            SecretCommand::Rm(_) => "secret rm",
            SecretCommand::Set(_) => "secret set",
        },
        Command::Resume(_) => "resume",
        Command::Rewind(_) => "rewind",
        Command::Fork(_) => "fork",
        Command::Wait(_) => "wait",
        Command::Workflow { command } => match command {
            WorkflowCommand::List(_) => "workflow list",
            WorkflowCommand::Create(_) => "workflow create",
        },
        Command::Skill { command } => match command {
            SkillCommand::Install(_) => "skill install",
        },
        Command::Discord => "discord",
        Command::Docs => "docs",
        Command::Upgrade(_) => "upgrade",
        Command::Provider { command } => match command {
            ProviderCommand::Login(_) => "provider login",
        },
        Command::System { command } => match command {
            SystemCommand::Prune(_) => "system prune",
            SystemCommand::Df(_) => "system df",
        },
        Command::SendAnalytics { .. } => "__send_analytics",
        Command::SendPanic { .. } => "__send_panic",
    };

    let command_name = command_name.to_string();

    let (config_log_level, upgrade_check_enabled) = {
        #[cfg(feature = "server")]
        {
            if let Command::Serve(ref args) = cli.command {
                match fabro_config::server::load_server_config(args.config.as_deref()) {
                    Ok(server_config) => (server_config.log.level, false),
                    Err(err) => return (command_name, Err(err)),
                }
            } else {
                match fabro_config::cli::load_cli_config(None) {
                    Ok(cli_config) => (cli_config.log.level, cli_config.upgrade_check),
                    Err(err) => return (command_name, Err(err)),
                }
            }
        }
        #[cfg(not(feature = "server"))]
        {
            match fabro_config::cli::load_cli_config(None) {
                Ok(cli_config) => (cli_config.log.level, cli_config.upgrade_check),
                Err(err) => return (command_name, Err(err)),
            }
        }
    };

    let log_prefix = if command_name == "serve" {
        "serve"
    } else {
        "cli"
    };
    if let Err(err) = logging::init_tracing(cli.debug, config_log_level.as_deref(), log_prefix) {
        eprintln!("Warning: failed to initialize logging: {err:#}");
    }

    debug!(command = %command_name, "CLI command started");

    let upgrade_handle = if matches!(
        cli.command,
        Command::Run(_)
            | Command::Create(_)
            | Command::Exec(_)
            | Command::Repo { .. }
            | Command::Init
            | Command::Install { .. }
    ) {
        upgrade::spawn_upgrade_check(cli.no_upgrade_check, upgrade_check_enabled)
    } else {
        None
    };

    let result = async {
        match cli.command {
            Command::Llm { command } => {
                let cli_config = cli_config::load_cli_config(None)?;
                let llm_defaults = cli_config.run_defaults.llm.as_ref();
                match command {
                    LlmCommand::Prompt(mut args) => {
                        if args.model.is_none() {
                            args.model = llm_defaults.and_then(|l| l.model.clone());
                        }
                        #[cfg(feature = "server")]
                        {
                            let resolved = cli_config::resolve_mode(
                                cli.mode,
                                cli.server_url.as_deref(),
                                &cli_config,
                            );
                            match resolved.mode {
                                cli_config::ExecutionMode::Server => {
                                    let client =
                                        cli_config::build_server_client(resolved.tls.as_ref())?;
                                    let server = fabro_llm::cli::ServerConnection {
                                        client,
                                        base_url: resolved.server_base_url,
                                    };
                                    fabro_llm::cli::run_prompt_via_server(args, &server).await?
                                }
                                cli_config::ExecutionMode::Standalone => {
                                    fabro_llm::cli::run_prompt(args).await?
                                }
                            }
                        }
                        #[cfg(not(feature = "server"))]
                        {
                            fabro_llm::cli::run_prompt(args).await?
                        }
                    }
                    LlmCommand::Chat(mut args) => {
                        if args.model.is_none() {
                            args.model = llm_defaults.and_then(|l| l.model.clone());
                        }
                        #[cfg(feature = "server")]
                        {
                            let resolved = cli_config::resolve_mode(
                                cli.mode,
                                cli.server_url.as_deref(),
                                &cli_config,
                            );
                            match resolved.mode {
                                cli_config::ExecutionMode::Server => {
                                    let client =
                                        cli_config::build_server_client(resolved.tls.as_ref())?;
                                    let server = fabro_llm::cli::ServerConnection {
                                        client,
                                        base_url: resolved.server_base_url,
                                    };
                                    fabro_llm::cli::run_chat_via_server(args, &server).await?
                                }
                                cli_config::ExecutionMode::Standalone => {
                                    fabro_llm::cli::run_chat(args).await?
                                }
                            }
                        }
                        #[cfg(not(feature = "server"))]
                        {
                            fabro_llm::cli::run_chat(args).await?
                        }
                    }
                }
            }
            Command::Exec(mut args) => {
                let cli_config = cli_config::load_cli_config(None)?;
                #[cfg(feature = "sleep_inhibitor")]
                let _sleep_guard = fabro_beastie::guard(cli_config.prevent_idle_sleep);
                let exec_defaults = cli_config.exec.as_ref();
                args.apply_cli_defaults(
                    exec_defaults.and_then(|a| a.provider.as_deref()),
                    exec_defaults.and_then(|a| a.model.as_deref()),
                    exec_defaults.and_then(|a| a.permissions),
                    exec_defaults.and_then(|a| a.output_format),
                );
                #[cfg(feature = "server")]
                let resolved =
                    cli_config::resolve_mode(cli.mode, cli.server_url.as_deref(), &cli_config);
                let mcp_servers: Vec<fabro_mcp::config::McpServerConfig> = cli_config
                    .run_defaults
                    .mcp_servers
                    .into_iter()
                    .map(|(name, entry)| entry.into_config(name))
                    .collect();
                #[cfg(feature = "server")]
                {
                    match resolved.mode {
                        cli_config::ExecutionMode::Server => {
                            tracing::info!(mode = "server", "Agent session starting");
                            let http_client =
                                cli_config::build_server_client(resolved.tls.as_ref())?;
                            let provider_name = args
                                .provider
                                .clone()
                                .unwrap_or_else(|| "anthropic".to_string());
                            let adapter =
                                std::sync::Arc::new(fabro_llm::providers::FabroServerAdapter::new(
                                    http_client,
                                    &resolved.server_base_url,
                                    &provider_name,
                                ));
                            let mut client = fabro_llm::client::Client::new(
                                std::collections::HashMap::new(),
                                None,
                                vec![],
                            );
                            client.register_provider(adapter).await.map_err(|e| {
                                anyhow::anyhow!("Failed to register fabro server adapter: {e}")
                            })?;
                            fabro_agent::cli::run_with_args_and_client(
                                args,
                                Some(client),
                                mcp_servers,
                            )
                            .await?
                        }
                        cli_config::ExecutionMode::Standalone => {
                            tracing::info!(mode = "standalone", "Agent session starting");
                            fabro_agent::cli::run_with_args(args, mcp_servers).await?
                        }
                    }
                }
                #[cfg(not(feature = "server"))]
                {
                    tracing::info!(mode = "standalone", "Agent session starting");
                    fabro_agent::cli::run_with_args(args, mcp_servers).await?
                }
            }
            Command::Run(mut args) => {
                let styles: &'static fabro_util::terminal::Styles =
                    Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
                let cli_config = cli_config::load_cli_config(None)?;
                args.verbose = args.verbose || cli_config.verbose;

                if args.detach {
                    // Detach mode: create + start + print run ID
                    let (run_id, run_dir) =
                        commands::create::create_run(&args, cli_config.run_defaults, styles, true)
                            .await?;
                    commands::start::start_run(&run_dir)?;
                    println!("{run_id}");
                } else {
                    // Foreground mode: use existing run_command
                    let github_app = build_github_app_credentials(cli_config.app_id());
                    let git_author = fabro_workflows::git::GitAuthor::from_options(
                        cli_config.git_author().and_then(|a| a.name.clone()),
                        cli_config.git_author().and_then(|a| a.email.clone()),
                    );

                    #[cfg(feature = "sleep_inhibitor")]
                    let _sleep_guard = fabro_beastie::guard(cli_config.prevent_idle_sleep);

                    commands::run::run_command(
                        args,
                        cli_config.run_defaults,
                        styles,
                        github_app,
                        git_author,
                    )
                    .await?;
                }
            }
            Command::Create(args) => {
                let styles: &'static fabro_util::terminal::Styles =
                    Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
                let cli_config = cli_config::load_cli_config(None)?;
                let (run_id, _run_dir) =
                    commands::create::create_run(&args, cli_config.run_defaults, styles, true)
                        .await?;
                println!("{run_id}");
            }
            Command::Start { run } => {
                let base = fabro_workflows::run_lookup::default_runs_base();
                let run_info = fabro_workflows::run_lookup::resolve_run(&base, &run)?;
                let pid = commands::start::start_run(&run_info.path)?;
                eprintln!("Started engine process (PID {pid})");
            }
            Command::Attach { run } => {
                let styles: &'static fabro_util::terminal::Styles =
                    Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
                let base = fabro_workflows::run_lookup::default_runs_base();
                let run_info = fabro_workflows::run_lookup::resolve_run(&base, &run)?;
                let exit_code = commands::attach::attach_run(&run_info.path, false, styles).await?;
                if exit_code != std::process::ExitCode::SUCCESS {
                    std::process::exit(1);
                }
            }
            Command::RunEngine { run_dir } => {
                let styles: &'static fabro_util::terminal::Styles =
                    Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
                let cli_config = cli_config::load_cli_config(None)?;
                let github_app = build_github_app_credentials(cli_config.app_id());
                let git_author = fabro_workflows::git::GitAuthor::from_options(
                    cli_config.git_author().and_then(|a| a.name.clone()),
                    cli_config.git_author().and_then(|a| a.email.clone()),
                );

                // Load spec and reconstruct RunArgs
                let spec = fabro_workflows::run_spec::RunSpec::load(&run_dir)?;

                // Restore the working directory captured at create time
                std::env::set_current_dir(&spec.working_directory).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to set working directory to {}: {e}",
                        spec.working_directory.display()
                    )
                })?;

                // Prefer the cached run.toml. prepare_workflow() falls back to the
                // sibling graph snapshot for older detached runs that predate run.toml.
                let workflow_path = commands::run::cached_run_config_path(&run_dir);

                let run_args = commands::run::RunArgs {
                    workflow: Some(workflow_path),
                    run_dir: Some(run_dir),
                    dry_run: spec.dry_run,
                    preflight: false,
                    auto_approve: spec.auto_approve,
                    goal: spec.goal,
                    goal_file: None,
                    model: Some(spec.model),
                    provider: Some(spec.provider.unwrap_or_default()).filter(|s| !s.is_empty()),
                    verbose: spec.verbose,
                    sandbox: spec
                        .sandbox_provider
                        .parse::<fabro_workflows::sandbox_provider::SandboxProvider>()
                        .ok()
                        .map(commands::run::CliSandboxProvider::from),
                    label: spec
                        .labels
                        .into_iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect(),
                    no_retro: spec.no_retro,
                    preserve_sandbox: spec.preserve_sandbox,
                    detach: false,
                    run_id: Some(spec.run_id),
                };

                commands::run::run_command(
                    run_args,
                    cli_config.run_defaults,
                    styles,
                    github_app,
                    git_author,
                )
                .await?;
            }
            Command::Validate(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                commands::validate::run(&args, &styles)?;
            }
            Command::Graph(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                commands::graph::run(&args, &styles)?;
            }
            Command::Parse(args) => {
                commands::parse::run(&args)?;
            }
            Command::Asset { command } => match command {
                AssetCommand::List(args) => {
                    commands::asset::list_command(&args)?;
                }
                AssetCommand::Cp(args) => {
                    commands::asset::cp_command(&args)?;
                }
            },
            Command::Cp(args) => {
                commands::cp::cp_command(args).await?;
            }
            Command::Preview(args) => {
                commands::preview::run(args).await?;
            }
            Command::Ssh(args) => {
                commands::ssh::run(args).await?;
            }
            Command::Diff(args) => {
                commands::diff::run(args).await?;
            }
            Command::Logs(args) => {
                let styles = fabro_util::terminal::Styles::detect_stdout();
                commands::logs::run(args, &styles)?;
            }
            Command::Inspect(args) => {
                commands::inspect::run(&args)?;
            }
            Command::Model { command } => {
                let server = {
                    #[cfg(feature = "server")]
                    {
                        let cli_config = cli_config::load_cli_config(None)?;
                        let resolved = cli_config::resolve_mode(
                            cli.mode,
                            cli.server_url.as_deref(),
                            &cli_config,
                        );
                        match resolved.mode {
                            cli_config::ExecutionMode::Server => {
                                let client =
                                    cli_config::build_server_client(resolved.tls.as_ref())?;
                                Some(fabro_llm::cli::ServerConnection {
                                    client,
                                    base_url: resolved.server_base_url,
                                })
                            }
                            cli_config::ExecutionMode::Standalone => None,
                        }
                    }
                    #[cfg(not(feature = "server"))]
                    {
                        None
                    }
                };
                fabro_llm::cli::run_models(command, server).await?
            }
            #[cfg(feature = "server")]
            Command::Serve(args) => {
                let styles: &'static fabro_util::terminal::Styles =
                    Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
                fabro_api::serve::serve_command(args, styles).await?;
            }
            Command::Doctor { verbose, dry_run } => {
                let cli_config = cli_config::load_cli_config(None)?;
                let verbose = verbose || cli_config.verbose;
                let exit_code = doctor::run_doctor(verbose, !dry_run).await;
                std::process::exit(exit_code);
            }
            Command::Discord => {
                open::that("https://fabro.sh/discord")?;
            }
            Command::Docs => {
                open::that("https://docs.fabro.sh/")?;
            }
            Command::Repo { command } => match command {
                RepoCommand::Init { skill } => {
                    init::run_init().await?;
                    if skill {
                        let base = std::env::current_dir()?.join(".claude").join("skills");
                        skill::install_skill_to(&base)?;
                    }
                }
                RepoCommand::Deinit => {
                    init::run_deinit()?;
                }
            },
            Command::Init => {
                eprintln!(
                    "{} `fabro init` is deprecated, use `fabro repo init` instead",
                    console::Style::new().yellow().apply_to("warning:")
                );
                init::run_init().await?;
            }
            Command::Install { web_url } => {
                install::run_install(&web_url).await?;
            }
            Command::Ps(args) => {
                let styles = fabro_util::terminal::Styles::detect_stdout();
                commands::runs::list_command(&args, &styles)?;
            }
            Command::Rm(args) => {
                commands::runs::remove_command(&args).await?;
            }
            Command::Pr { command } => {
                let cli_config = cli_config::load_cli_config(None)?;
                let github_app = build_github_app_credentials(cli_config.app_id());
                match command {
                    PrCommand::Create(args) => {
                        commands::pr::create_command(args, github_app).await?;
                    }
                    PrCommand::List(args) => {
                        commands::pr::list_command(args, github_app).await?;
                    }
                    PrCommand::View(args) => {
                        commands::pr::view_command(args, github_app).await?;
                    }
                    PrCommand::Merge(args) => {
                        commands::pr::merge_command(args, github_app).await?;
                    }
                    PrCommand::Close(args) => {
                        commands::pr::close_command(args, github_app).await?;
                    }
                }
            }
            Command::Secret { command } => match command {
                SecretCommand::Get(args) => {
                    commands::secret::get_command(&args)?;
                }
                SecretCommand::List(args) => {
                    commands::secret::list_command(&args)?;
                }
                SecretCommand::Rm(args) => {
                    commands::secret::rm_command(&args)?;
                }
                SecretCommand::Set(args) => {
                    commands::secret::set_command(&args)?;
                }
            },
            Command::Resume(mut args) => {
                let styles: &'static fabro_util::terminal::Styles =
                    Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
                let cli_config = cli_config::load_cli_config(None)?;
                args.verbose = args.verbose || cli_config.verbose;
                #[cfg(feature = "sleep_inhibitor")]
                let _sleep_guard = fabro_beastie::guard(cli_config.prevent_idle_sleep);
                let github_app = build_github_app_credentials(cli_config.app_id());
                let git_author = fabro_workflows::git::GitAuthor::from_options(
                    cli_config.git_author().and_then(|a| a.name.clone()),
                    cli_config.git_author().and_then(|a| a.email.clone()),
                );
                commands::resume::resume_command(
                    args,
                    cli_config.run_defaults,
                    styles,
                    github_app,
                    git_author,
                )
                .await?;
            }
            Command::Rewind(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                commands::rewind::run(&args, &styles)?;
            }
            Command::Fork(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                commands::fork::run(&args, &styles)?;
            }
            Command::Wait(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                commands::wait::run(args, &styles)?;
            }
            Command::Workflow { command } => match command {
                WorkflowCommand::List(args) => {
                    commands::workflow::list_command(&args)?;
                }
                WorkflowCommand::Create(args) => {
                    commands::workflow::create_command(&args)?;
                }
            },
            Command::Skill { command } => match command {
                SkillCommand::Install(args) => {
                    skill::run_skill_install(&args)?;
                }
            },
            Command::Upgrade(args) => {
                upgrade::run_upgrade(args).await?;
            }
            Command::Provider { command } => match command {
                ProviderCommand::Login(args) => {
                    commands::provider::login_command(args).await?;
                }
            },
            Command::System { command } => match command {
                SystemCommand::Prune(args) => {
                    commands::runs::prune_command(&args)?;
                }
                SystemCommand::Df(args) => {
                    commands::runs::df_command(&args)?;
                }
            },
            Command::SendAnalytics { path } => {
                let result = fabro_telemetry::sender::upload(&path).await;
                let _ = std::fs::remove_file(&path);
                result?;
            }
            Command::SendPanic { path } => {
                let result = fabro_telemetry::panic::capture(&path).await;
                let _ = std::fs::remove_file(&path);
                result?;
            }
        }

        Ok(())
    }
    .await;

    // Print upgrade notice after command completes (non-blocking during execution)
    if let Some(handle) = upgrade_handle {
        let _ = handle.await;
    }

    (command_name, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_provider_login_openai() {
        let cli = Cli::try_parse_from(["fabro", "provider", "login", "--provider", "openai"])
            .expect("should parse");
        match cli.command {
            Command::Provider {
                command: ProviderCommand::Login(args),
            } => {
                assert_eq!(args.provider, fabro_model::Provider::OpenAi);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_provider_login_anthropic() {
        let cli = Cli::try_parse_from(["fabro", "provider", "login", "--provider", "anthropic"])
            .expect("should parse");
        match cli.command {
            Command::Provider {
                command: ProviderCommand::Login(args),
            } => {
                assert_eq!(args.provider, fabro_model::Provider::Anthropic);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_provider_login_missing_provider_flag() {
        let result = Cli::try_parse_from(["fabro", "provider", "login"]);
        assert!(result.is_err(), "should fail without --provider");
    }

    #[test]
    fn parse_provider_login_bogus_provider() {
        let result = Cli::try_parse_from(["fabro", "provider", "login", "--provider", "bogus"]);
        assert!(result.is_err(), "should fail with unknown provider");
    }

    #[test]
    fn parse_create_command() {
        let cli = Cli::try_parse_from(["fabro", "create", "my-workflow.toml", "--goal", "test"])
            .expect("should parse");
        match cli.command {
            Command::Create(args) => {
                assert_eq!(
                    args.workflow.as_deref(),
                    Some(std::path::Path::new("my-workflow.toml"))
                );
                assert_eq!(args.goal.as_deref(), Some("test"));
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_start_command() {
        let cli = Cli::try_parse_from(["fabro", "start", "ABC123"]).expect("should parse");
        match cli.command {
            Command::Start { run } => {
                assert_eq!(run, "ABC123");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_attach_command() {
        let cli = Cli::try_parse_from(["fabro", "attach", "ABC123"]).expect("should parse");
        match cli.command {
            Command::Attach { run } => {
                assert_eq!(run, "ABC123");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_run_engine_command() {
        let cli = Cli::try_parse_from(["fabro", "_run_engine", "--run-dir", "/tmp/runs/test"])
            .expect("should parse");
        match cli.command {
            Command::RunEngine { run_dir } => {
                assert_eq!(run_dir, std::path::PathBuf::from("/tmp/runs/test"));
            }
            _ => panic!("unexpected command variant"),
        }
    }
}
