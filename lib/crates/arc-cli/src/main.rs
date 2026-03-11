mod cli_config;
mod doctor;
mod init;
mod install;
mod logging;
mod skill;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::debug;

#[derive(Parser)]
#[command(name = "arc", version, long_version = arc_util::version::LONG_VERSION.as_str())]
struct Cli {
    /// Enable DEBUG-level logging (default is INFO)
    #[arg(long, global = true)]
    debug: bool,

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
    Exec(arc_agent::cli::AgentArgs),
    /// Launch a workflow run
    Run(arc_workflows::cli::RunArgs),
    /// Validate a workflow
    Validate(arc_workflows::cli::ValidateArgs),
    /// Render a workflow graph as SVG or PNG
    Graph(arc_workflows::cli::graph::GraphArgs),
    /// Parse a DOT file and print its AST
    #[command(hide = true)]
    Parse(arc_workflows::cli::ParseArgs),
    /// Copy files to/from a run's sandbox
    Cp(arc_workflows::cli::cp::CpArgs),
    /// Get a preview URL for a port on a run's sandbox
    Preview(arc_workflows::cli::preview::PreviewArgs),
    /// SSH into a run's Daytona sandbox
    Ssh(arc_workflows::cli::ssh::SshArgs),
    /// Show the diff of changes from a workflow run
    #[command(hide = true)]
    Diff(arc_workflows::cli::diff::DiffArgs),
    /// List and test LLM models
    Model {
        #[command(subcommand)]
        command: Option<arc_llm::cli::ModelsCommand>,
    },
    /// Start the HTTP API server
    #[cfg(feature = "server")]
    Serve(arc_api::serve::ServeArgs),
    /// Check environment and integration health
    Doctor {
        /// Show detailed information for each check
        #[arg(short, long)]
        verbose: bool,

        /// Skip live service probes (LLM, sandbox, API, web, Brave Search)
        #[arg(long)]
        dry_run: bool,
    },
    /// Initialize a new arc project
    Init,
    /// Set up the Arc environment (LLMs, certs, GitHub)
    Install,
    /// List workflow runs
    #[command(hide = true)]
    Ps(arc_workflows::cli::runs::RunsListArgs),
    /// Pull request operations
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Skill management
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
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
}

#[derive(Subcommand)]
enum PrCommand {
    /// Create a pull request from a completed run
    Create(arc_workflows::cli::pr::PrCreateArgs),
    /// List pull requests from workflow runs
    List(arc_workflows::cli::pr::PrListArgs),
    /// View pull request details
    View(arc_workflows::cli::pr::PrViewArgs),
    /// Merge a pull request
    Merge(arc_workflows::cli::pr::PrMergeArgs),
    /// Close a pull request
    Close(arc_workflows::cli::pr::PrCloseArgs),
}

#[derive(Subcommand)]
enum SystemCommand {
    /// Delete old workflow runs
    Prune(arc_workflows::cli::runs::RunsPruneArgs),
    /// Show disk usage
    Df(arc_workflows::cli::runs::DfArgs),
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Install a built-in skill
    Install(skill::SkillInstallArgs),
}

#[derive(Subcommand)]
enum LlmCommand {
    /// Execute a prompt
    Prompt(arc_llm::cli::PromptArgs),
    /// Interactive multi-turn chat
    Chat(arc_llm::cli::ChatArgs),
}

pub(crate) fn build_github_app_credentials(
    app_id: Option<&str>,
) -> Option<arc_github::GitHubAppCredentials> {
    let app_id = app_id?;
    let raw = std::env::var("GITHUB_APP_PRIVATE_KEY").ok()?;
    let private_key_pem = if raw.starts_with("-----") {
        raw
    } else {
        let pem_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &raw).ok()?;
        String::from_utf8(pem_bytes).ok()?
    };
    Some(arc_github::GitHubAppCredentials {
        app_id: app_id.to_string(),
        private_key_pem,
    })
}

#[tokio::main]
async fn main() {
    let start = std::time::Instant::now();
    let raw_args: Vec<String> = std::env::args().collect();

    let (command_name, result) = main_inner().await;
    let duration_ms = start.elapsed().as_millis() as u64;

    send_telemetry_event(&raw_args, &command_name, duration_ms, &result);

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

fn send_telemetry_event(
    raw_args: &[String],
    command_name: &str,
    duration_ms: u64,
    result: &Result<()>,
) {
    let is_error = result.is_err();
    let telemetry = match arc_util::telemetry::Telemetry::for_cli() {
        Ok(t) => t,
        Err(err) => {
            debug!(%err, "Telemetry initialization failed");
            return;
        }
    };
    if !telemetry.should_track(is_error) {
        return;
    }

    let event_name = if is_error {
        "Command Error"
    } else {
        "Command Run"
    };
    let properties = serde_json::json!({
        "subcommand": command_name,
        "command": arc_util::telemetry::sanitize::sanitize_command(raw_args, command_name),
        "durationMs": duration_ms,
        "repository": arc_util::telemetry::git::repository_identifier(),
        "ci": std::env::var("CI").is_ok(),
        "success": !is_error,
        "exitCode": if is_error { 1 } else { 0 },
    });

    let track = telemetry.build_track(event_name, properties);
    arc_util::telemetry::sender::send(track);
    debug!(
        event = event_name,
        subcommand = command_name,
        "Telemetry event queued"
    );
}

async fn main_inner() -> (String, Result<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    if let Some(home) = dirs::home_dir() {
        let env_path = home.join(".arc").join(".env");
        if dotenvy::from_path(&env_path).is_ok() {
            debug!(path = %env_path.display(), "Loaded environment file");
        }
    }

    let command_name = match &cli.command {
        Command::Llm { command } => match command {
            LlmCommand::Prompt(_) => "llm prompt",
            LlmCommand::Chat(_) => "llm chat",
        },
        Command::Exec(_) => "exec",
        Command::Run(_) => "run",
        Command::Validate(_) => "validate",
        Command::Graph(_) => "graph",
        Command::Parse(_) => "parse",
        Command::Cp(_) => "cp",
        Command::Preview(_) => "preview",
        Command::Ssh(_) => "ssh",
        Command::Diff(_) => "diff",
        Command::Model { command } => match command {
            Some(arc_llm::cli::ModelsCommand::List { .. }) => "model list",
            Some(arc_llm::cli::ModelsCommand::Test { .. }) => "model test",
            None => "model",
        },
        #[cfg(feature = "server")]
        Command::Serve(_) => "serve",
        Command::Doctor { .. } => "doctor",
        Command::Init => "init",
        Command::Install => "install",
        Command::Ps(_) => "ps",
        Command::Pr { command } => match command {
            PrCommand::Create(_) => "pr create",
            PrCommand::List(_) => "pr list",
            PrCommand::View(_) => "pr view",
            PrCommand::Merge(_) => "pr merge",
            PrCommand::Close(_) => "pr close",
        },
        Command::Skill { command } => match command {
            SkillCommand::Install(_) => "skill install",
        },
        Command::System { command } => match command {
            SystemCommand::Prune(_) => "system prune",
            SystemCommand::Df(_) => "system df",
        },
        Command::SendAnalytics { .. } => "__send_analytics",
    };

    let command_name = command_name.to_string();

    let config_log_level = {
        #[cfg(feature = "server")]
        {
            if let Command::Serve(ref args) = cli.command {
                match arc_config::server::load_server_config(args.config.as_deref()) {
                    Ok(server_config) => server_config.log.level,
                    Err(err) => return (command_name, Err(err)),
                }
            } else {
                match arc_config::cli::load_cli_config(None) {
                    Ok(cli_config) => cli_config.log.level,
                    Err(err) => return (command_name, Err(err)),
                }
            }
        }
        #[cfg(not(feature = "server"))]
        {
            match arc_config::cli::load_cli_config(None) {
                Ok(cli_config) => cli_config.log.level,
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
                                    let server = arc_llm::cli::ServerConnection {
                                        client,
                                        base_url: resolved.server_base_url,
                                    };
                                    arc_llm::cli::run_prompt_via_server(args, &server).await?
                                }
                                cli_config::ExecutionMode::Standalone => {
                                    arc_llm::cli::run_prompt(args).await?
                                }
                            }
                        }
                        #[cfg(not(feature = "server"))]
                        {
                            arc_llm::cli::run_prompt(args).await?
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
                                    let server = arc_llm::cli::ServerConnection {
                                        client,
                                        base_url: resolved.server_base_url,
                                    };
                                    arc_llm::cli::run_chat_via_server(args, &server).await?
                                }
                                cli_config::ExecutionMode::Standalone => {
                                    arc_llm::cli::run_chat(args).await?
                                }
                            }
                        }
                        #[cfg(not(feature = "server"))]
                        {
                            arc_llm::cli::run_chat(args).await?
                        }
                    }
                }
            }
            Command::Exec(mut args) => {
                let cli_config = cli_config::load_cli_config(None)?;
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
                let mcp_servers: Vec<arc_mcp::config::McpServerConfig> = cli_config
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
                                std::sync::Arc::new(arc_llm::providers::ArcServerAdapter::new(
                                    http_client,
                                    &resolved.server_base_url,
                                    &provider_name,
                                ));
                            let mut client = arc_llm::client::Client::new(
                                std::collections::HashMap::new(),
                                None,
                                vec![],
                            );
                            client.register_provider(adapter).await.map_err(|e| {
                                anyhow::anyhow!("Failed to register arc server adapter: {e}")
                            })?;
                            arc_agent::cli::run_with_args_and_client(
                                args,
                                Some(client),
                                mcp_servers,
                            )
                            .await?
                        }
                        cli_config::ExecutionMode::Standalone => {
                            tracing::info!(mode = "standalone", "Agent session starting");
                            arc_agent::cli::run_with_args(args, mcp_servers).await?
                        }
                    }
                }
                #[cfg(not(feature = "server"))]
                {
                    tracing::info!(mode = "standalone", "Agent session starting");
                    arc_agent::cli::run_with_args(args, mcp_servers).await?
                }
            }
            Command::Run(mut args) => {
                let styles: &'static arc_util::terminal::Styles =
                    Box::leak(Box::new(arc_util::terminal::Styles::detect_stderr()));
                let cli_config = cli_config::load_cli_config(None)?;
                args.verbose = args.verbose || cli_config.verbose;
                let github_app = build_github_app_credentials(cli_config.app_id());

                let git_author = arc_workflows::git::GitAuthor::from_options(
                    cli_config.git_author().and_then(|a| a.name.clone()),
                    cli_config.git_author().and_then(|a| a.email.clone()),
                );

                arc_workflows::cli::run::run_command(
                    args,
                    cli_config.run_defaults,
                    styles,
                    github_app,
                    git_author,
                )
                .await?;
            }
            Command::Validate(args) => {
                let styles = arc_util::terminal::Styles::detect_stderr();
                arc_workflows::cli::validate::validate_command(&args, &styles)?;
            }
            Command::Graph(args) => {
                let styles = arc_util::terminal::Styles::detect_stderr();
                arc_workflows::cli::graph::graph_command(&args, &styles)?;
            }
            Command::Parse(args) => {
                arc_workflows::cli::parse::parse_command(&args)?;
            }
            Command::Cp(args) => {
                arc_workflows::cli::cp::cp_command(args).await?;
            }
            Command::Preview(args) => {
                arc_workflows::cli::preview::preview_command(args).await?;
            }
            Command::Ssh(args) => {
                arc_workflows::cli::ssh::ssh_command(args).await?;
            }
            Command::Diff(args) => {
                arc_workflows::cli::diff::diff_command(args).await?;
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
                                Some(arc_llm::cli::ServerConnection {
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
                arc_llm::cli::run_models(command, server).await?
            }
            #[cfg(feature = "server")]
            Command::Serve(args) => {
                let styles: &'static arc_util::terminal::Styles =
                    Box::leak(Box::new(arc_util::terminal::Styles::detect_stderr()));
                arc_api::serve::serve_command(args, styles).await?;
            }
            Command::Doctor { verbose, dry_run } => {
                let cli_config = cli_config::load_cli_config(None)?;
                let verbose = verbose || cli_config.verbose;
                let exit_code = doctor::run_doctor(verbose, !dry_run).await;
                std::process::exit(exit_code);
            }
            Command::Init => {
                init::run_init().await?;
            }
            Command::Install => {
                install::run_install().await?;
            }
            Command::Ps(args) => {
                arc_workflows::cli::runs::list_command(&args)?;
            }
            Command::Pr { command } => {
                let cli_config = cli_config::load_cli_config(None)?;
                let github_app = build_github_app_credentials(cli_config.app_id());
                match command {
                    PrCommand::Create(args) => {
                        arc_workflows::cli::pr::pr_create_command(args, github_app).await?;
                    }
                    PrCommand::List(args) => {
                        arc_workflows::cli::pr::pr_list_command(args, github_app).await?;
                    }
                    PrCommand::View(args) => {
                        arc_workflows::cli::pr::pr_view_command(args, github_app).await?;
                    }
                    PrCommand::Merge(args) => {
                        arc_workflows::cli::pr::pr_merge_command(args, github_app).await?;
                    }
                    PrCommand::Close(args) => {
                        arc_workflows::cli::pr::pr_close_command(args, github_app).await?;
                    }
                }
            }
            Command::Skill { command } => match command {
                SkillCommand::Install(args) => {
                    skill::run_skill_install(&args)?;
                }
            },
            Command::System { command } => match command {
                SystemCommand::Prune(args) => {
                    arc_workflows::cli::runs::prune_command(&args)?;
                }
                SystemCommand::Df(args) => {
                    arc_workflows::cli::runs::df_command(&args)?;
                }
            },
            Command::SendAnalytics { path } => {
                let result = async {
                    let json = std::fs::read(&path)?;
                    let track: arc_util::telemetry::event::Track = serde_json::from_slice(&json)?;
                    arc_util::telemetry::sender::send_to_segment(&track).await
                }
                .await;
                let _ = std::fs::remove_file(&path);
                result?;
            }
        }

        Ok(())
    }
    .await;

    (command_name, result)
}
