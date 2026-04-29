#![allow(
    clippy::exit,
    reason = "The CLI exits explicitly with the computed process status."
)]

mod args;
mod command_context;
mod commands;
mod gh;
mod landing;
mod local_server;
mod logging;
mod manifest_builder;
mod server_client;
mod server_runs;
mod shared;
#[cfg(feature = "sleep_inhibitor")]
mod sleep_inhibitor;
mod user_config;

use std::fmt::{self, Debug, Display};

use anyhow::Result;
use args::{
    Cli, Commands, RunCommands, ServerCommand, ServerNamespace, global_args_cli_layer,
    require_no_json_override,
};
use clap::CommandFactory;
use fabro_static::EnvVars;
use fabro_telemetry::{git, panic as tel_panic, sanitize, sender};
use fabro_types::settings::LogDestination;
use fabro_util::exit::ExitClass;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_util::{browser, exit};
use rustls::crypto::ring::default_provider;
use tracing::debug;

use crate::command_context::CommandContext;

#[expect(clippy::print_stderr, reason = "fatal error reporting before exit")]
#[tokio::main]
async fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    let subcommand = raw_args.get(1).map(String::as_str);
    let subcommand_arg = raw_args.get(2).map(String::as_str);
    if subcommand == Some("__render-graph") && !matches!(subcommand_arg, Some("--help" | "-h")) {
        std::process::exit(commands::render_graph::execute());
    }

    // Capture the worker bearer token immediately and scrub it from the process
    // env before any subprocess can be spawned. Every descendant of the worker
    // (hooks, sandbox commands, devcontainer setup, MCP stdio, etc.) therefore
    // inherits a process env that no longer contains FABRO_WORKER_TOKEN, so an
    // unscrubbed spawn site cannot leak it. The token flows to `runner::execute`
    // through an explicit function argument instead of the environment.
    let worker_token = if subcommand == Some("__run-worker") {
        let token = process_env_var(EnvVars::FABRO_WORKER_TOKEN);
        #[expect(
            clippy::disallowed_methods,
            reason = "Scrub the worker bearer from this process's env before any \
                      child process is spawned, so no descendant can inherit it."
        )]
        {
            std::env::remove_var(EnvVars::FABRO_WORKER_TOKEN);
        }
        token
    } else {
        None
    };

    install_miette_hook();

    tel_panic::install_panic_hook();
    fabro_telemetry::init_cli();

    let start = std::time::Instant::now();

    let (command_name, result) = Box::pin(main_inner(worker_token)).await;
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let exit_code = result.as_ref().err().map_or(0, exit::exit_code_for);

    let is_error = result.is_err();
    // An empty command_name means no subcommand was invoked (landing was shown);
    // don't emit a tracking event for that case.
    if !command_name.is_empty() {
        let command = sanitize::sanitize_command(&raw_args, &command_name);
        let repository = git::repository_identifier();
        let ci = process_env_var(EnvVars::CI).is_some();
        if is_error {
            fabro_telemetry::track!("CLI Errored", {
                "subcommand": command_name,
                "command": command,
                "durationMs": duration_ms,
                "repository": repository,
                "ci": ci,
                "success": false,
                "exitCode": exit_code,
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
    }
    fabro_telemetry::shutdown();

    if let Err(err) = result {
        let json_mode = raw_args.iter().any(|a| a == "--json");
        eprintln!(
            "{:?}",
            miette::Report::new(CliDiagnostic::new(err, !json_mode))
        );
        std::process::exit(exit_code);
    }
}

fn install_miette_hook() {
    let _ = miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .with_cause_chain()
                .wrap_lines(false)
                .break_words(false)
                .build(),
        )
    }));
}

struct CliDiagnostic {
    err:            anyhow::Error,
    show_auth_hint: bool,
}

impl CliDiagnostic {
    fn new(err: anyhow::Error, show_auth_hint: bool) -> Self {
        Self {
            err,
            show_auth_hint,
        }
    }
}

impl Display for CliDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.err, formatter)
    }
}

impl Debug for CliDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.err, formatter)
    }
}

impl std::error::Error for CliDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.err.source()
    }
}

impl miette::Diagnostic for CliDiagnostic {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        if self.show_auth_hint && exit::exit_class_for(&self.err) == Some(ExitClass::AuthRequired) {
            Some(Box::new("Run `fabro auth login` to authenticate."))
        } else {
            None
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "CLI main reads documented process-env controls before telemetry and worker dispatch."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

async fn main_inner(worker_token: Option<String>) -> (String, Result<()>) {
    let _ = default_provider().install_default();

    let cli = Cli::parse();

    let Cli { globals, command } = cli;
    let Some(command) = command else {
        landing::print();
        return (String::new(), Ok(()));
    };
    let bootstrap_printer = Printer::from_flags(globals.quiet, globals.verbose);
    let cli_layer = global_args_cli_layer(&globals);
    let process_local_json = globals.json;
    let command_name = command.name().to_string();
    let pre_tracing_bootstrap = match pre_tracing_bootstrap(command.as_ref()).await {
        Ok(bootstrap) => bootstrap,
        Err(err) => return (command_name, Err(err)),
    };

    let base_ctx = match CommandContext::from_disk(&cli_layer, process_local_json) {
        Ok(ctx) => ctx,
        Err(err) => return (command_name, Err(err)),
    };
    let printer = base_ctx.printer();

    let config_log_level = match &pre_tracing_bootstrap.sink {
        logging::InternalLogSink::Cli => base_ctx.user_settings().cli.logging.level.clone(),
        logging::InternalLogSink::Server { .. } | logging::InternalLogSink::Worker { .. } => {
            pre_tracing_bootstrap.config_log_level.clone()
        }
    };
    if let Err(err) = logging::init_tracing(
        globals.debug,
        config_log_level.as_deref(),
        &pre_tracing_bootstrap.sink,
    ) {
        fabro_util::printerr!(
            bootstrap_printer,
            "Warning: failed to initialize logging: {err:#}"
        );
    }

    debug!(command = %command_name, "CLI command started");
    let foreground_server_log_bootstrap = pre_tracing_bootstrap.foreground_server_log_bootstrap;

    let upgrade_handle = if matches!(
        command.as_ref(),
        Commands::RunCmd(RunCommands::Run(_) | RunCommands::Create(_))
            | Commands::Exec(_)
            | Commands::Repo(_)
            | Commands::Install { .. }
    ) {
        commands::upgrade::spawn_upgrade_check(base_ctx.user_settings().cli.updates.check, printer)
    } else {
        None
    };

    let result = Box::pin(async move {
        match *command {
            Commands::Exec(args) => {
                commands::exec::execute(args, &base_ctx).await?;
            }
            Commands::RunCmd(cmd) => {
                Box::pin(commands::run::dispatch(cmd, &base_ctx, worker_token)).await?;
            }
            Commands::Preflight(args) => {
                commands::preflight::execute(args, &base_ctx).await?;
            }
            Commands::Validate(args) => {
                let styles = Styles::detect_stderr();
                commands::validate::run(&args, &styles, &base_ctx)?;
            }
            Commands::Graph(args) => {
                let styles = Styles::detect_stderr();
                commands::graph::run(&args, &styles, &base_ctx).await?;
            }
            Commands::Parse(args) => {
                commands::parse::run(&args)?;
            }
            Commands::Artifact(ns) => {
                commands::artifact::dispatch(ns, &base_ctx).await?;
            }
            Commands::Dump(args) => {
                commands::dump::run(&args, &base_ctx).await?;
            }
            Commands::RunsCmd(cmd) => {
                commands::runs::dispatch(cmd, &base_ctx).await?;
            }
            Commands::Model { command } => {
                commands::model::execute(command, &base_ctx).await?;
            }
            Commands::Server(ns) => {
                Box::pin(commands::server::dispatch(
                    ns.command,
                    &globals,
                    foreground_server_log_bootstrap,
                    printer,
                ))
                .await?;
            }
            Commands::Doctor(args) => {
                let exit_code = Box::pin(commands::doctor::run_doctor(&args, &base_ctx)).await?;
                std::process::exit(exit_code);
            }
            Commands::Version(args) => {
                commands::version::version_command(&args, &base_ctx).await?;
            }
            Commands::Discord => {
                if process_local_json {
                    shared::print_json_pretty(&serde_json::json!({
                        "url": "https://fabro.sh/discord",
                    }))?;
                } else {
                    browser::try_open("https://fabro.sh/discord")?;
                }
            }
            Commands::Docs => {
                if process_local_json {
                    shared::print_json_pretty(&serde_json::json!({
                        "url": "https://docs.fabro.sh/",
                    }))?;
                } else {
                    browser::try_open("https://docs.fabro.sh/")?;
                }
            }
            Commands::Repo(ns) => {
                commands::repo::dispatch(ns, &base_ctx).await?;
            }
            Commands::Install { args, command } => {
                Box::pin(commands::install::execute(&args, command, &base_ctx)).await?;
            }
            Commands::Uninstall(args) => {
                commands::uninstall::run_uninstall(&args, &base_ctx).await?;
            }
            Commands::Auth(ns) => {
                commands::auth::dispatch(ns, &base_ctx).await?;
            }
            Commands::Pr(ns) => {
                Box::pin(commands::pr::dispatch(ns, &base_ctx)).await?;
            }
            Commands::Secret(ns) => {
                commands::secret::dispatch(ns, &base_ctx).await?;
            }
            Commands::Settings(args) => {
                Box::pin(commands::config::execute(&args, &base_ctx)).await?;
            }
            Commands::Workflow(ns) => {
                commands::workflow::dispatch(ns, &base_ctx)?;
            }
            Commands::Upgrade(args) => {
                commands::upgrade::run_upgrade(args, &base_ctx).await?;
            }
            Commands::Provider(ns) => {
                commands::provider::dispatch(ns, &base_ctx).await?;
            }
            Commands::Sandbox { command } => {
                commands::sandbox::dispatch(command, &base_ctx).await?;
            }
            Commands::System(ns) => {
                commands::system::dispatch(ns, &base_ctx).await?;
            }
            Commands::Completion(args) => {
                require_no_json_override(process_local_json)?;
                let mut cmd = Cli::command();
                let shell = args.shell;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut buf = Vec::new();
                    clap_complete::generate(shell, &mut cmd, "fabro", &mut buf);
                    buf
                }));
                match result {
                    Ok(buf) => {
                        #[expect(
                            clippy::disallowed_types,
                            clippy::disallowed_methods,
                            reason = "sync CLI: stream shell completions to stdout"
                        )]
                        {
                            use std::io::Write;
                            std::io::stdout().write_all(&buf)?;
                        }
                    }
                    Err(_) => {
                        anyhow::bail!(
                            "Failed to generate completions for {shell}. \
                             Try zsh, fish, elvish, or powershell instead."
                        );
                    }
                }
            }
            Commands::SendAnalytics { path } => {
                let result = sender::upload(&path).await;
                let _ = std::fs::remove_file(&path);
                result?;
            }
            Commands::SendPanic { path } => {
                let result = tel_panic::capture(&path);
                let _ = std::fs::remove_file(&path);
                result?;
            }
            Commands::RenderGraph => unreachable!("__render-graph handled before CLI bootstrap"),
            #[cfg(debug_assertions)]
            Commands::TestPanic { message } => {
                let event = tel_panic::build_event(&message);
                let json = serde_json::to_string_pretty(&event)?;
                fabro_util::printout!(printer, "{json}");
            }
        }

        Ok(())
    })
    .await;

    // Print upgrade notice after command completes (non-blocking during execution)
    if let Some(handle) = upgrade_handle {
        let _ = handle.await;
    }

    (command_name, result)
}

struct PreTracingBootstrap {
    sink: logging::InternalLogSink,
    config_log_level: Option<String>,
    foreground_server_log_bootstrap: Option<commands::server::start::ForegroundServerLogBootstrap>,
}

impl PreTracingBootstrap {
    fn cli() -> Self {
        Self {
            sink: logging::InternalLogSink::Cli,
            config_log_level: None,
            foreground_server_log_bootstrap: None,
        }
    }
}

async fn pre_tracing_bootstrap(command: &Commands) -> Result<PreTracingBootstrap> {
    match command {
        Commands::Server(ServerNamespace {
            command: ServerCommand::Start(args),
        }) if args.foreground => {
            prepare_server_bootstrap(
                args.serve_args.config.as_deref(),
                args.storage_dir.as_deref(),
                true,
            )
            .await
        }
        Commands::Server(ServerNamespace {
            command: ServerCommand::Restart(args),
        }) if args.foreground => {
            prepare_server_bootstrap(
                args.serve_args.config.as_deref(),
                args.storage_dir.as_deref(),
                true,
            )
            .await
        }
        Commands::Server(ServerNamespace {
            command: ServerCommand::Serve(args),
        }) => {
            prepare_server_bootstrap(
                args.serve_args.config.as_deref(),
                args.storage_dir.as_deref(),
                false,
            )
            .await
        }
        Commands::RunCmd(RunCommands::RunWorker(args)) => {
            prepare_run_worker_bootstrap(args.storage_dir.as_deref(), &args.run_dir)
        }
        _ => Ok(PreTracingBootstrap::cli()),
    }
}

async fn prepare_server_bootstrap(
    config_path: Option<&std::path::Path>,
    storage_dir: Option<&std::path::Path>,
    foreground: bool,
) -> Result<PreTracingBootstrap> {
    let local_config = local_server::LocalServerConfig::load(config_path, storage_dir)?;
    let storage_dir = local_config.storage_dir().to_path_buf();
    let runtime_directory = fabro_config::RuntimeDirectory::new(storage_dir.clone());
    let log_destination = fabro_config::resolve_log_destination(
        local_config.config_log_destination().unwrap_or_default(),
    )?;
    let foreground_server_log_bootstrap = if foreground {
        Some(
            commands::server::start::prepare_foreground_server_log(
                &runtime_directory,
                log_destination,
            )
            .await?,
        )
    } else {
        None
    };
    let log = log_sink(log_destination, &runtime_directory);

    Ok(PreTracingBootstrap {
        sink: logging::InternalLogSink::Server { log },
        config_log_level: local_config.config_log_level().map(str::to_owned),
        foreground_server_log_bootstrap,
    })
}

fn prepare_run_worker_bootstrap(
    storage_dir: Option<&std::path::Path>,
    run_dir: &std::path::Path,
) -> Result<PreTracingBootstrap> {
    let local_config = local_server::LocalServerConfig::load_with_storage_dir(storage_dir)?;
    let runtime_directory = fabro_config::RuntimeDirectory::new(local_config.storage_dir());
    let log_destination = fabro_config::resolve_log_destination(
        local_config.config_log_destination().unwrap_or_default(),
    )?;
    let server_log = log_sink(log_destination, &runtime_directory);

    Ok(PreTracingBootstrap {
        sink: logging::InternalLogSink::Worker {
            server_log,
            per_run_log_path: run_dir.join("runtime").join("server.log"),
        },
        config_log_level: None,
        foreground_server_log_bootstrap: None,
    })
}

fn log_sink(
    destination: LogDestination,
    runtime_directory: &fabro_config::RuntimeDirectory,
) -> logging::LogSink {
    match destination {
        LogDestination::File => logging::LogSink::File(runtime_directory.log_path()),
        LogDestination::Stdout => logging::LogSink::Stdout,
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "main.rs tests stage CLI settings fixtures with sync std::fs::write"
)]
mod tests {
    use args::{
        AuthCommand, AuthNamespace, Commands, InstallGitHubStrategyArg, ModelsCommand,
        ProviderCommand, ProviderNamespace,
    };
    use temp_env::with_var;
    use tokio::runtime::Runtime;

    use super::*;

    fn runtime() -> Runtime {
        Runtime::new().expect("runtime should build")
    }

    fn write_test_settings(path: &std::path::Path) {
        write_test_settings_with_logging(path, "warn", "stdout");
    }

    fn write_test_settings_with_logging(path: &std::path::Path, level: &str, destination: &str) {
        std::fs::write(
            path,
            format!(
                r#"
_version = 1

[server.logging]
level = "{level}"
destination = "{destination}"
"#
            ),
        )
        .unwrap();
    }

    fn expect_bootstrap_err(result: Result<PreTracingBootstrap>) -> anyhow::Error {
        match result {
            Ok(_) => panic!("bootstrap should have failed"),
            Err(err) => err,
        }
    }

    #[test]
    fn pre_tracing_bootstrap_uses_cli_sink_for_normal_cli_command() {
        let cli = Cli::try_parse_from(["fabro", "uninstall"]).expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let bootstrap = runtime()
            .block_on(pre_tracing_bootstrap(command))
            .expect("bootstrap should resolve");

        assert_eq!(bootstrap.sink, logging::InternalLogSink::Cli);
        assert!(bootstrap.config_log_level.is_none());
        assert!(bootstrap.foreground_server_log_bootstrap.is_none());
    }

    #[test]
    fn parse_provider_login_openai() {
        let cli = Cli::try_parse_from(["fabro", "provider", "login", "--provider", "openai"])
            .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Provider(ProviderNamespace {
                command: ProviderCommand::Login(args),
            }) => {
                assert_eq!(args.provider, fabro_model::Provider::OpenAi);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_provider_login_anthropic() {
        let cli = Cli::try_parse_from(["fabro", "provider", "login", "--provider", "anthropic"])
            .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Provider(ProviderNamespace {
                command: ProviderCommand::Login(args),
            }) => {
                assert_eq!(args.provider, fabro_model::Provider::Anthropic);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_provider_login_api_key_stdin() {
        let cli = Cli::try_parse_from([
            "fabro",
            "provider",
            "login",
            "--provider",
            "anthropic",
            "--api-key-stdin",
        ])
        .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Provider(ProviderNamespace {
                command: ProviderCommand::Login(args),
            }) => {
                assert_eq!(args.provider, fabro_model::Provider::Anthropic);
                assert!(args.api_key_stdin);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_auth_logout_all() {
        let cli = Cli::try_parse_from(["fabro", "auth", "logout", "--all"]).expect("should parse");
        match *cli.command.unwrap() {
            Commands::Auth(AuthNamespace {
                command: AuthCommand::Logout(args),
            }) => {
                assert!(args.all);
                assert!(args.server.server.is_none());
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_install_non_interactive_accepts_token_strategy() {
        let cli = Cli::try_parse_from([
            "fabro",
            "install",
            "--non-interactive",
            "--llm-provider",
            "anthropic",
            "--llm-api-key-env",
            "ANTHROPIC_API_KEY",
            "--github-strategy",
            "token",
            "--github-username",
            "brynary",
        ])
        .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Install {
                args,
                command: None,
            } => {
                assert!(args.non_interactive);
                assert_eq!(
                    args.scripted.github_strategy,
                    Some(InstallGitHubStrategyArg::Token)
                );
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_install_github_non_interactive_accepts_token_strategy() {
        let cli = Cli::try_parse_from([
            "fabro",
            "install",
            "github",
            "--non-interactive",
            "--strategy",
            "token",
        ])
        .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Install {
                args,
                command: Some(args::InstallCommand::Github(github_args)),
            } => {
                assert!(args.non_interactive);
                assert_eq!(github_args.strategy, Some(InstallGitHubStrategyArg::Token));
                assert_eq!(github_args.owner, None);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn pre_tracing_bootstrap_uses_server_sink_for_server_start_foreground() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings(&config_path);

        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "start",
            "--foreground",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let bootstrap = runtime()
            .block_on(pre_tracing_bootstrap(command))
            .expect("bootstrap should resolve");

        assert_eq!(bootstrap.sink, logging::InternalLogSink::Server {
            log: logging::LogSink::Stdout,
        });
        assert_eq!(bootstrap.config_log_level.as_deref(), Some("warn"));
        assert!(bootstrap.foreground_server_log_bootstrap.is_some());
    }

    #[test]
    fn pre_tracing_bootstrap_uses_server_sink_for_server_restart_foreground() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings(&config_path);

        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "restart",
            "--foreground",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let bootstrap = runtime()
            .block_on(pre_tracing_bootstrap(command))
            .expect("bootstrap should resolve");

        assert_eq!(bootstrap.sink, logging::InternalLogSink::Server {
            log: logging::LogSink::Stdout,
        });
        assert_eq!(bootstrap.config_log_level.as_deref(), Some("warn"));
        assert!(bootstrap.foreground_server_log_bootstrap.is_some());
    }

    #[test]
    fn pre_tracing_bootstrap_uses_server_sink_for_server_serve() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings(&config_path);
        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "__serve",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let bootstrap = runtime()
            .block_on(pre_tracing_bootstrap(command))
            .expect("bootstrap should resolve");

        assert_eq!(bootstrap.sink, logging::InternalLogSink::Server {
            log: logging::LogSink::Stdout,
        });
        assert_eq!(bootstrap.config_log_level.as_deref(), Some("warn"));
        assert!(bootstrap.foreground_server_log_bootstrap.is_none());
    }

    #[test]
    fn pre_tracing_bootstrap_env_destination_overrides_config_file() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings_with_logging(&config_path, "warn", "file");

        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "start",
            "--foreground",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        with_var(EnvVars::FABRO_LOG_DESTINATION, Some("stdout"), || {
            let bootstrap = runtime()
                .block_on(pre_tracing_bootstrap(command))
                .expect("bootstrap should resolve");

            assert_eq!(bootstrap.sink, logging::InternalLogSink::Server {
                log: logging::LogSink::Stdout,
            });
        });
    }

    #[test]
    fn pre_tracing_bootstrap_rejects_invalid_env_destination() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings_with_logging(&config_path, "warn", "file");

        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "start",
            "--foreground",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        with_var(EnvVars::FABRO_LOG_DESTINATION, Some("stdot"), || {
            let err = expect_bootstrap_err(runtime().block_on(pre_tracing_bootstrap(command)));
            let message = err.to_string();
            assert!(message.contains(EnvVars::FABRO_LOG_DESTINATION));
            assert!(message.contains("stdot"));
        });
    }

    #[test]
    fn pre_tracing_bootstrap_rejects_invalid_config_log_level() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings_with_logging(&config_path, "definitely not a filter", "file");

        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "start",
            "--foreground",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let err = expect_bootstrap_err(runtime().block_on(pre_tracing_bootstrap(command)));
        assert!(
            err.to_string().contains("server.logging.level"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn pre_tracing_bootstrap_rejects_invalid_config_destination() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings_with_logging(&config_path, "warn", "stdot");

        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "start",
            "--foreground",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let err = expect_bootstrap_err(runtime().block_on(pre_tracing_bootstrap(command)));
        assert!(
            err.to_string().contains("server.logging.destination"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn pre_tracing_bootstrap_uses_worker_sink_for_run_worker() {
        let storage_dir = tempfile::tempdir().unwrap();
        let run_dir = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "fabro",
            "__run-worker",
            "--server",
            "/tmp/fabro.sock",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--run-dir",
            run_dir.path().to_str().unwrap(),
            "--run-id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--mode",
            "start",
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let bootstrap = runtime()
            .block_on(pre_tracing_bootstrap(command))
            .expect("bootstrap should resolve");

        assert_eq!(bootstrap.sink, logging::InternalLogSink::Worker {
            server_log:       logging::LogSink::File(
                storage_dir.path().join("logs").join("server.log"),
            ),
            per_run_log_path: run_dir.path().join("runtime").join("server.log"),
        });
        assert!(bootstrap.config_log_level.is_none());
        assert!(bootstrap.foreground_server_log_bootstrap.is_none());
    }

    #[test]
    fn pre_tracing_bootstrap_worker_uses_stdout_when_env_overrides() {
        let storage_dir = tempfile::tempdir().unwrap();
        let run_dir = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "fabro",
            "__run-worker",
            "--server",
            "/tmp/fabro.sock",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--run-dir",
            run_dir.path().to_str().unwrap(),
            "--run-id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--mode",
            "start",
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        with_var(EnvVars::FABRO_LOG_DESTINATION, Some("stdout"), || {
            let bootstrap = runtime()
                .block_on(pre_tracing_bootstrap(command))
                .expect("bootstrap should resolve");

            assert_eq!(bootstrap.sink, logging::InternalLogSink::Worker {
                server_log:       logging::LogSink::Stdout,
                per_run_log_path: run_dir.path().join("runtime").join("server.log"),
            });
        });
    }

    #[test]
    fn pre_tracing_bootstrap_uses_cli_sink_for_server_start_daemon_wrapper() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings(&config_path);

        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "start",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let bootstrap = runtime()
            .block_on(pre_tracing_bootstrap(command))
            .expect("bootstrap should resolve");

        assert_eq!(bootstrap.sink, logging::InternalLogSink::Cli);
        assert!(bootstrap.config_log_level.is_none());
        assert!(bootstrap.foreground_server_log_bootstrap.is_none());
    }

    #[test]
    fn pre_tracing_bootstrap_uses_cli_sink_for_server_restart_daemon_wrapper() {
        let storage_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("settings.toml");
        write_test_settings(&config_path);

        let cli = Cli::try_parse_from([
            "fabro",
            "server",
            "restart",
            "--storage-dir",
            storage_dir.path().to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .expect("should parse");
        let command = cli.command.as_deref().unwrap();

        let bootstrap = runtime()
            .block_on(pre_tracing_bootstrap(command))
            .expect("bootstrap should resolve");

        assert_eq!(bootstrap.sink, logging::InternalLogSink::Cli);
        assert!(bootstrap.config_log_level.is_none());
        assert!(bootstrap.foreground_server_log_bootstrap.is_none());
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
        match *cli.command.unwrap() {
            Commands::RunCmd(RunCommands::Create(args)) => {
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
    fn parse_run_storage_dir_after_subcommand_is_rejected() {
        let result = Cli::try_parse_from([
            "fabro",
            "run",
            "test/simple.fabro",
            "--storage-dir",
            "/tmp/fabro",
        ]);
        assert!(result.is_err(), "should reject run --storage-dir");
    }

    #[test]
    fn parse_model_list_server_target_after_subcommand() {
        let cli = Cli::try_parse_from([
            "fabro",
            "model",
            "list",
            "--server",
            "http://localhost:3000/api/v1",
        ])
        .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Model {
                command: Some(ModelsCommand::List(args)),
            } => assert_eq!(args.target.as_deref(), Some("http://localhost:3000/api/v1")),
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_exec_server_target_after_subcommand() {
        let cli = Cli::try_parse_from([
            "fabro",
            "exec",
            "--server",
            "http://localhost:3000/api/v1",
            "fix the bug",
        ])
        .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Exec(args) => {
                assert_eq!(args.server.as_deref(), Some("http://localhost:3000/api/v1"));
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_model_server_target_conflicts_with_storage_dir() {
        let result = Cli::try_parse_from([
            "fabro",
            "model",
            "list",
            "--storage-dir",
            "/tmp/fabro",
            "--server",
            "http://localhost:3000",
        ]);
        assert!(
            result.is_err(),
            "should fail with conflicting model target flags"
        );
    }

    #[test]
    fn parse_global_server_target_before_subcommand_is_rejected() {
        let result = Cli::try_parse_from([
            "fabro",
            "--server",
            "http://localhost:3000/api/v1",
            "model",
            "list",
        ]);
        assert!(result.is_err(), "should reject top-level --server");
    }

    #[test]
    fn parse_global_storage_dir_before_subcommand_is_rejected() {
        let result = Cli::try_parse_from([
            "fabro",
            "--storage-dir",
            "/tmp/fabro",
            "run",
            "test/simple.fabro",
        ]);
        assert!(result.is_err(), "should reject top-level --storage-dir");
    }

    #[test]
    fn parse_upgrade_prerelease_conflicts_with_version() {
        use clap::error::ErrorKind;
        let err = Cli::try_parse_from(["fabro", "upgrade", "--prerelease", "--version", "0.1.0"])
            .err()
            .expect("should reject --prerelease combined with --version");
        assert_eq!(
            err.kind(),
            ErrorKind::ArgumentConflict,
            "expected ArgumentConflict, got {:?}: {err}",
            err.kind()
        );
    }

    #[test]
    fn parse_dump_command() {
        let cli =
            Cli::try_parse_from(["fabro", "dump", "ABC123", "-o", "./out"]).expect("should parse");
        match *cli.command.unwrap() {
            Commands::Dump(args) => {
                assert_eq!(args.run, "ABC123");
                assert_eq!(args.output, std::path::PathBuf::from("./out"));
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_start_command() {
        let cli = Cli::try_parse_from(["fabro", "start", "ABC123"]).expect("should parse");
        match *cli.command.unwrap() {
            Commands::RunCmd(RunCommands::Start(args)) => {
                assert_eq!(args.run, "ABC123");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_attach_command() {
        let cli = Cli::try_parse_from(["fabro", "attach", "ABC123"]).expect("should parse");
        match *cli.command.unwrap() {
            Commands::RunCmd(RunCommands::Attach(args)) => {
                assert_eq!(args.run, "ABC123");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_sandbox_cp_command() {
        let cli = Cli::try_parse_from(["fabro", "sandbox", "cp", "ABC123:/tmp/file", "./file"])
            .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Sandbox {
                command: args::SandboxCommand::Cp(args),
            } => {
                assert_eq!(args.src, "ABC123:/tmp/file");
                assert_eq!(args.dst, "./file");
                assert!(!args.recursive);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_run_worker_command() {
        let cli = Cli::try_parse_from([
            "fabro",
            "__run-worker",
            "--server",
            "/tmp/fabro.sock",
            "--run-dir",
            "/tmp/run",
            "--run-id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--mode",
            "start",
        ])
        .expect("should parse");
        match *cli.command.unwrap() {
            Commands::RunCmd(RunCommands::RunWorker(args)) => {
                assert_eq!(args.server, "/tmp/fabro.sock");
                assert_eq!(args.run_dir, std::path::PathBuf::from("/tmp/run"));
                assert_eq!(args.run_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap());
                assert!(matches!(args.mode, args::RunWorkerMode::Start));
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_run_worker_with_resume_mode() {
        let cli = Cli::try_parse_from([
            "fabro",
            "__run-worker",
            "--server",
            "http://127.0.0.1:3000",
            "--run-dir",
            "/tmp/run",
            "--run-id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--mode",
            "resume",
        ])
        .expect("should parse");
        match *cli.command.unwrap() {
            Commands::RunCmd(RunCommands::RunWorker(args)) => {
                assert_eq!(args.server, "http://127.0.0.1:3000");
                assert_eq!(args.run_dir, std::path::PathBuf::from("/tmp/run"));
                assert_eq!(args.run_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap());
                assert!(matches!(args.mode, args::RunWorkerMode::Resume));
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_render_graph_command() {
        let cli = Cli::try_parse_from(["fabro", "__render-graph"]).expect("should parse");
        match *cli.command.unwrap() {
            Commands::RenderGraph => {}
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_settings_command() {
        let cli = Cli::try_parse_from(["fabro", "settings"]).expect("should parse");
        assert_eq!(cli.command.as_ref().unwrap().name(), "settings");
        match *cli.command.unwrap() {
            Commands::Settings(args) => {
                assert!(args.target.server.is_none());
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_settings_rejects_workflow_argument() {
        let result = Cli::try_parse_from(["fabro", "settings", "demo"]);
        assert!(result.is_err(), "should reject settings workflow argument");
    }

    #[test]
    fn parse_settings_rejects_local_flag() {
        let result = Cli::try_parse_from(["fabro", "settings", "--local"]);
        assert!(result.is_err(), "should reject settings --local");
    }

    #[test]
    fn parse_quiet_flag() {
        let cli = Cli::try_parse_from(["fabro", "--quiet", "settings"]).expect("should parse");
        assert!(cli.globals.quiet);
        assert!(!cli.globals.verbose);
    }

    #[test]
    fn parse_verbose_flag() {
        let cli = Cli::try_parse_from(["fabro", "--verbose", "settings"]).expect("should parse");
        assert!(!cli.globals.quiet);
        assert!(cli.globals.verbose);
    }

    #[test]
    fn quiet_and_verbose_conflict() {
        let result = Cli::try_parse_from(["fabro", "--quiet", "--verbose", "settings"]);
        assert!(
            result.is_err(),
            "should fail when both --quiet and --verbose"
        );
    }
}
