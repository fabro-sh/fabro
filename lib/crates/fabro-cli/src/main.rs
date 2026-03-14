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
    Run(fabro_workflows::cli::RunArgs),
    /// Validate a workflow
    Validate(fabro_workflows::cli::ValidateArgs),
    /// Render a workflow graph as SVG or PNG
    Graph(fabro_workflows::cli::graph::GraphArgs),
    /// Parse a DOT file and print its AST
    #[command(hide = true)]
    Parse(fabro_workflows::cli::ParseArgs),
    /// Copy files to/from a run's sandbox
    Cp(fabro_workflows::cli::cp::CpArgs),
    /// Get a preview URL for a port on a run's sandbox
    Preview(fabro_workflows::cli::preview::PreviewArgs),
    /// SSH into a run's Daytona sandbox
    Ssh(fabro_workflows::cli::ssh::SshArgs),
    /// Show the diff of changes from a workflow run
    #[command(hide = true)]
    Diff(fabro_workflows::cli::diff::DiffArgs),
    /// View the event log of a workflow run
    Logs(fabro_workflows::cli::logs::LogsArgs),
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
    /// Initialize a new project
    Init,
    /// Set up the Fabro environment (LLMs, certs, GitHub)
    Install,
    /// List workflow runs
    #[command(hide = true)]
    Ps(fabro_workflows::cli::runs::RunsListArgs),
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
    /// Rewind a workflow run to an earlier checkpoint
    Rewind(fabro_workflows::cli::rewind::RewindArgs),
    /// Workflow operations
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
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
    Create(fabro_workflows::cli::pr::PrCreateArgs),
    /// List pull requests from workflow runs
    List(fabro_workflows::cli::pr::PrListArgs),
    /// View pull request details
    View(fabro_workflows::cli::pr::PrViewArgs),
    /// Merge a pull request
    Merge(fabro_workflows::cli::pr::PrMergeArgs),
    /// Close a pull request
    Close(fabro_workflows::cli::pr::PrCloseArgs),
}

#[derive(Subcommand)]
enum SystemCommand {
    /// Delete old workflow runs
    Prune(fabro_workflows::cli::runs::RunsPruneArgs),
    /// Show disk usage
    Df(fabro_workflows::cli::runs::DfArgs),
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Install a built-in skill
    Install(skill::SkillInstallArgs),
}

#[derive(Subcommand)]
enum WorkflowCommand {
    /// List available workflows
    List(fabro_workflows::cli::workflow::WorkflowListArgs),
    /// Create a new workflow
    Create(fabro_workflows::cli::workflow::WorkflowCreateArgs),
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

/// Fork the workflow as a background process, print the run ID, and exit.
fn detach_run(args: fabro_workflows::cli::RunArgs) -> Result<()> {
    let run_id = ulid::Ulid::new().to_string();

    let run_dir = args.run_dir.clone().unwrap_or_else(|| {
        let base = dirs::home_dir()
            .expect("could not determine home directory")
            .join(".fabro")
            .join("runs");
        base.join(format!(
            "{}-{}",
            chrono::Local::now().format("%Y%m%d"),
            run_id
        ))
    });
    std::fs::create_dir_all(&run_dir)?;

    let log_file = std::fs::File::create(run_dir.join("detach.log"))?;

    // Rebuild argv: current exe + original args, stripping --detach/-d, injecting --run-id and --run-dir
    let exe = std::env::current_exe()?;
    let mut child_args: Vec<String> = Vec::new();
    child_args.push("run".to_string());

    let raw_args: Vec<String> = std::env::args().collect();
    // Skip argv[0] (binary) and argv[1] ("run"), then filter out --detach / -d
    let mut iter = raw_args.iter().skip(2).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--detach" || arg == "-d" {
            continue;
        }
        // Skip --run-dir and its value (we'll override it)
        if arg == "--run-dir" {
            iter.next(); // consume the value
            continue;
        }
        if arg.starts_with("--run-dir=") {
            continue;
        }
        // Skip --run-id and its value (we'll override it)
        if arg == "--run-id" {
            iter.next();
            continue;
        }
        if arg.starts_with("--run-id=") {
            continue;
        }
        child_args.push(arg.clone());
    }
    child_args.push("--run-id".to_string());
    child_args.push(run_id.clone());
    child_args.push("--run-dir".to_string());
    child_args.push(run_dir.to_string_lossy().to_string());

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(&child_args)
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .stdin(std::process::Stdio::null());

    // Detach from the controlling terminal on unix
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    cmd.spawn()?;
    println!("{run_id}");
    Ok(())
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
    let telemetry = match fabro_util::telemetry::Telemetry::for_cli() {
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
        "command": fabro_util::telemetry::sanitize::sanitize_command(raw_args, command_name),
        "durationMs": duration_ms,
        "repository": fabro_util::telemetry::git::repository_identifier(),
        "ci": std::env::var("CI").is_ok(),
        "success": !is_error,
        "exitCode": if is_error { 1 } else { 0 },
    });

    let track = telemetry.build_track(event_name, properties);
    fabro_util::telemetry::sender::send(track);
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
        Command::Exec(_) => "exec",
        Command::Run(_) => "run",
        Command::Validate(_) => "validate",
        Command::Graph(_) => "graph",
        Command::Parse(_) => "parse",
        Command::Cp(_) => "cp",
        Command::Preview(_) => "preview",
        Command::Ssh(_) => "ssh",
        Command::Diff(_) => "diff",
        Command::Logs(_) => "logs",
        Command::Model { command } => match command {
            Some(fabro_llm::cli::ModelsCommand::List { .. }) => "model list",
            Some(fabro_llm::cli::ModelsCommand::Test { .. }) => "model test",
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
        Command::Rewind(_) => "rewind",
        Command::Workflow { command } => match command {
            WorkflowCommand::List(_) => "workflow list",
            WorkflowCommand::Create(_) => "workflow create",
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
                match fabro_config::server::load_server_config(args.config.as_deref()) {
                    Ok(server_config) => server_config.log.level,
                    Err(err) => return (command_name, Err(err)),
                }
            } else {
                match fabro_config::cli::load_cli_config(None) {
                    Ok(cli_config) => cli_config.log.level,
                    Err(err) => return (command_name, Err(err)),
                }
            }
        }
        #[cfg(not(feature = "server"))]
        {
            match fabro_config::cli::load_cli_config(None) {
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
                if args.detach {
                    return detach_run(args);
                }

                let styles: &'static fabro_util::terminal::Styles =
                    Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
                let cli_config = cli_config::load_cli_config(None)?;
                args.verbose = args.verbose || cli_config.verbose;
                let github_app = build_github_app_credentials(cli_config.app_id());

                let git_author = fabro_workflows::git::GitAuthor::from_options(
                    cli_config.git_author().and_then(|a| a.name.clone()),
                    cli_config.git_author().and_then(|a| a.email.clone()),
                );

                fabro_workflows::cli::run::run_command(
                    args,
                    cli_config.run_defaults,
                    styles,
                    github_app,
                    git_author,
                )
                .await?;
            }
            Command::Validate(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                fabro_workflows::cli::validate::validate_command(&args, &styles)?;
            }
            Command::Graph(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                fabro_workflows::cli::graph::graph_command(&args, &styles)?;
            }
            Command::Parse(args) => {
                fabro_workflows::cli::parse::parse_command(&args)?;
            }
            Command::Cp(args) => {
                fabro_workflows::cli::cp::cp_command(args).await?;
            }
            Command::Preview(args) => {
                fabro_workflows::cli::preview::preview_command(args).await?;
            }
            Command::Ssh(args) => {
                fabro_workflows::cli::ssh::ssh_command(args).await?;
            }
            Command::Diff(args) => {
                fabro_workflows::cli::diff::diff_command(args).await?;
            }
            Command::Logs(args) => {
                let styles = fabro_util::terminal::Styles::detect_stdout();
                fabro_workflows::cli::logs::logs_command(args, &styles)?;
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
            Command::Init => {
                init::run_init().await?;
            }
            Command::Install => {
                install::run_install().await?;
            }
            Command::Ps(args) => {
                fabro_workflows::cli::runs::list_command(&args)?;
            }
            Command::Pr { command } => {
                let cli_config = cli_config::load_cli_config(None)?;
                let github_app = build_github_app_credentials(cli_config.app_id());
                match command {
                    PrCommand::Create(args) => {
                        fabro_workflows::cli::pr::pr_create_command(args, github_app).await?;
                    }
                    PrCommand::List(args) => {
                        fabro_workflows::cli::pr::pr_list_command(args, github_app).await?;
                    }
                    PrCommand::View(args) => {
                        fabro_workflows::cli::pr::pr_view_command(args, github_app).await?;
                    }
                    PrCommand::Merge(args) => {
                        fabro_workflows::cli::pr::pr_merge_command(args, github_app).await?;
                    }
                    PrCommand::Close(args) => {
                        fabro_workflows::cli::pr::pr_close_command(args, github_app).await?;
                    }
                }
            }
            Command::Rewind(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                fabro_workflows::cli::rewind::rewind_command(&args, &styles)?;
            }
            Command::Workflow { command } => match command {
                WorkflowCommand::List(args) => {
                    fabro_workflows::cli::workflow::workflow_list_command(&args)?;
                }
                WorkflowCommand::Create(args) => {
                    fabro_workflows::cli::workflow::workflow_create_command(&args)?;
                }
            },
            Command::Skill { command } => match command {
                SkillCommand::Install(args) => {
                    skill::run_skill_install(&args)?;
                }
            },
            Command::System { command } => match command {
                SystemCommand::Prune(args) => {
                    fabro_workflows::cli::runs::prune_command(&args)?;
                }
                SystemCommand::Df(args) => {
                    fabro_workflows::cli::runs::df_command(&args)?;
                }
            },
            Command::SendAnalytics { path } => {
                let result = async {
                    let json = std::fs::read(&path)?;
                    let track: fabro_util::telemetry::event::Track = serde_json::from_slice(&json)?;
                    fabro_util::telemetry::sender::send_to_segment(&track).await
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
