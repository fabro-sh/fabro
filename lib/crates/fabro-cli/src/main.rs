#![allow(clippy::exit)]

mod args;
mod command_context;
mod commands;
mod gh;
mod landing;
mod logging;
mod manifest_builder;
mod server_client;
mod server_runs;
mod shared;
#[cfg(feature = "sleep_inhibitor")]
mod sleep_inhibitor;
mod sse;
mod user_config;

#[cfg(test)]
use std::ffi::OsString;

use anyhow::Result;
use args::{
    Commands, GlobalArgs, LONG_VERSION, RunCommands, ServerCommand, ServerNamespace,
    global_args_cli_layer, printer_from_verbosity, require_no_json_override,
};
use clap::{CommandFactory, Parser};
use fabro_config::merge::combine_files;
use fabro_config::user::load_settings_config;
use fabro_telemetry::{git, panic as tel_panic, sanitize, sender};
use fabro_types::settings::SettingsLayer;
use fabro_types::settings::cli::OutputVerbosity;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use rustls::crypto::ring::default_provider;
use tracing::debug;

#[derive(Parser)]
#[command(name = "fabro", version, long_version = LONG_VERSION)]
struct Cli {
    #[command(flatten)]
    globals: GlobalArgs,

    #[command(subcommand)]
    command: Option<Box<Commands>>,
}

impl Cli {
    fn parse() -> Self {
        <Self as Parser>::parse()
    }

    #[cfg(test)]
    fn try_parse_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        <Self as Parser>::try_parse_from(args)
    }
}

#[expect(clippy::print_stderr, reason = "fatal error reporting before exit")]
#[tokio::main]
async fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    let subcommand = raw_args.get(1).map(String::as_str);
    let subcommand_arg = raw_args.get(2).map(String::as_str);
    if subcommand == Some("__render-graph") && !matches!(subcommand_arg, Some("--help" | "-h")) {
        std::process::exit(commands::render_graph::execute());
    }

    tel_panic::install_panic_hook();
    fabro_telemetry::init_cli();

    let start = std::time::Instant::now();

    let (command_name, result) = Box::pin(main_inner()).await;
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap();

    let is_error = result.is_err();
    // An empty command_name means no subcommand was invoked (landing was shown);
    // don't emit a tracking event for that case.
    if !command_name.is_empty() {
        let command = sanitize::sanitize_command(&raw_args, &command_name);
        let repository = git::repository_identifier();
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

    let user_settings = match user_config::load_settings() {
        Ok(settings) => settings,
        Err(err) => return (command_name, Err(err)),
    };
    let combined_settings = combine_files(user_settings, SettingsLayer {
        cli: Some(cli_layer.clone()),
        ..SettingsLayer::default()
    });
    let cli_settings = match user_config::resolve_cli_settings(&combined_settings) {
        Ok(cli_settings) => cli_settings,
        Err(err) => return (command_name, Err(err)),
    };
    let printer = printer_from_verbosity(cli_settings.output.verbosity);

    let config_log_level = if let Commands::Server(ServerNamespace {
        command:
            ServerCommand::Start(args::ServerStartArgs {
                serve_args: args, ..
            })
            | ServerCommand::Serve(args::ServerServeArgs {
                serve_args: args, ..
            }),
    }) = command.as_ref()
    {
        match load_settings_config(args.config.as_deref()) {
            Ok(layer) => layer
                .server
                .as_ref()
                .and_then(|server| server.logging.as_ref())
                .and_then(|logging| logging.level.clone()),
            Err(err) => return (command_name, Err(err.into())),
        }
    } else {
        cli_settings.logging.level.clone()
    };

    let log_prefix = if command_name == "server start" || command_name == "server __serve" {
        "server"
    } else {
        "cli"
    };
    if let Err(err) = logging::init_tracing(globals.debug, config_log_level.as_deref(), log_prefix)
    {
        fabro_util::printerr!(
            bootstrap_printer,
            "Warning: failed to initialize logging: {err:#}"
        );
    }

    debug!(command = %command_name, "CLI command started");

    let upgrade_handle = if matches!(
        command.as_ref(),
        Commands::RunCmd(RunCommands::Run(_) | RunCommands::Create(_))
            | Commands::Exec(_)
            | Commands::Repo(_)
            | Commands::Install { .. }
    ) {
        commands::upgrade::spawn_upgrade_check(cli_settings.updates.check, printer)
    } else {
        None
    };

    let result = Box::pin(async move {
        match *command {
            Commands::Exec(args) => commands::exec::execute(args, &cli_settings, printer).await?,
            Commands::RunCmd(cmd) => {
                Box::pin(commands::run::dispatch(
                    cmd,
                    &cli_settings,
                    &cli_layer,
                    process_local_json,
                    printer,
                ))
                .await?;
            }
            Commands::Preflight(args) => {
                commands::preflight::execute(args, &cli_settings, &cli_layer, printer).await?;
            }
            Commands::Validate(args) => {
                let styles = Styles::detect_stderr();
                commands::validate::run(&args, &styles, &cli_settings, &cli_layer, printer).await?;
            }
            Commands::Graph(args) => {
                let styles = Styles::detect_stderr();
                commands::graph::run(
                    &args,
                    &styles,
                    &cli_settings,
                    &cli_layer,
                    process_local_json,
                    printer,
                )
                .await?;
            }
            Commands::Parse(args) => {
                commands::parse::run(&args, &cli_settings, printer)?;
            }
            Commands::Artifact(ns) => {
                commands::artifact::dispatch(ns, &cli_settings, &cli_layer, printer).await?;
            }
            Commands::Store(ns) => commands::store::dispatch(ns, &cli_settings, printer).await?,
            Commands::RunsCmd(cmd) => {
                commands::runs::dispatch(cmd, &cli_settings, &cli_layer, printer).await?;
            }
            Commands::Model { command } => {
                commands::model::execute(command, &cli_settings, &cli_layer, printer).await?;
            }
            Commands::Server(ns) => {
                Box::pin(commands::server::dispatch(ns.command, &globals, printer)).await?;
            }
            Commands::Doctor(args) => {
                let verbose =
                    args.verbose || cli_settings.output.verbosity == OutputVerbosity::Verbose;
                let exit_code = Box::pin(commands::doctor::run_doctor(
                    &args,
                    verbose,
                    &cli_settings,
                    &cli_layer,
                    printer,
                ))
                .await?;
                std::process::exit(exit_code);
            }
            Commands::Version(args) => {
                commands::version::version_command(&args, &cli_settings, &cli_layer, printer)
                    .await?;
            }
            Commands::Discord => {
                if process_local_json {
                    shared::print_json_pretty(&serde_json::json!({
                        "url": "https://fabro.sh/discord",
                    }))?;
                } else {
                    open::that("https://fabro.sh/discord")?;
                }
            }
            Commands::Docs => {
                if process_local_json {
                    shared::print_json_pretty(&serde_json::json!({
                        "url": "https://docs.fabro.sh/",
                    }))?;
                } else {
                    open::that("https://docs.fabro.sh/")?;
                }
            }
            Commands::Repo(ns) => {
                commands::repo::dispatch(ns, &cli_settings, &cli_layer, printer).await?;
            }
            Commands::Install { args, command } => {
                Box::pin(commands::install::execute(
                    &args,
                    command,
                    &cli_settings,
                    &cli_layer,
                    process_local_json,
                    printer,
                ))
                .await?;
            }
            Commands::Uninstall(args) => {
                commands::uninstall::run_uninstall(&args, &cli_settings, printer).await?;
            }
            Commands::Pr(ns) => {
                Box::pin(commands::pr::dispatch(
                    ns,
                    &cli_settings,
                    &cli_layer,
                    printer,
                ))
                .await?;
            }
            Commands::Secret(ns) => {
                commands::secret::dispatch(ns, &cli_settings, &cli_layer, printer).await?;
            }
            Commands::Settings(args) => {
                Box::pin(commands::config::execute(
                    &args,
                    &cli_settings,
                    &cli_layer,
                    printer,
                ))
                .await?;
            }
            Commands::Workflow(ns) => commands::workflow::dispatch(ns, &cli_settings, printer)?,
            Commands::Upgrade(args) => {
                commands::upgrade::run_upgrade(args, &cli_settings, printer).await?;
            }
            Commands::Provider(ns) => {
                commands::provider::dispatch(
                    ns,
                    &cli_settings,
                    &cli_layer,
                    process_local_json,
                    printer,
                )
                .await?;
            }
            Commands::Sandbox { command } => {
                commands::sandbox::dispatch(
                    command,
                    &cli_settings,
                    &cli_layer,
                    process_local_json,
                    printer,
                )
                .await?;
            }
            Commands::System(ns) => {
                commands::system::dispatch(ns, &cli_settings, &cli_layer, printer).await?;
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

#[cfg(test)]
mod tests {
    use args::{
        Commands, InstallGitHubStrategyArg, ModelsCommand, ProviderCommand, ProviderNamespace,
        StoreCommand, StoreNamespace,
    };

    use super::*;

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
    fn parse_store_dump_command() {
        let cli = Cli::try_parse_from(["fabro", "store", "dump", "ABC123", "-o", "./out"])
            .expect("should parse");
        match *cli.command.unwrap() {
            Commands::Store(StoreNamespace {
                command: StoreCommand::Dump(args),
            }) => {
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
            "--artifact-upload-token",
            "token-123",
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
                assert_eq!(args.artifact_upload_token.as_deref(), Some("token-123"));
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
                assert!(args.artifact_upload_token.is_none());
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
                assert!(!args.local);
                assert!(args.target.server.is_none());
                assert!(args.workflow.is_none());
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_settings_with_workflow() {
        let cli = Cli::try_parse_from(["fabro", "settings", "demo"]).expect("should parse");
        match *cli.command.unwrap() {
            Commands::Settings(args) => {
                assert_eq!(args.workflow, Some(std::path::PathBuf::from("demo")));
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_settings_local_mode() {
        let cli =
            Cli::try_parse_from(["fabro", "settings", "--local", "demo"]).expect("should parse");
        match *cli.command.unwrap() {
            Commands::Settings(args) => {
                assert!(args.local);
                assert_eq!(args.workflow, Some(std::path::PathBuf::from("demo")));
            }
            _ => panic!("unexpected command variant"),
        }
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
