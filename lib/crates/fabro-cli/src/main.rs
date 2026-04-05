#![allow(clippy::print_stdout, clippy::print_stderr, clippy::exit)]

mod args;
mod commands;
mod logging;
mod server_client;
mod server_runs;
mod shared;
#[cfg(feature = "sleep_inhibitor")]
mod sleep_inhibitor;
mod user_config;

use anyhow::Result;
use args::{Commands, GlobalArgs, LONG_VERSION, RunCommands, ServerCommand, ServerNamespace};
use clap::{CommandFactory, Parser};
use fabro_config::server::load_server_settings;
use fabro_telemetry::{git, panic as tel_panic, sanitize, sender};
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
    command: Box<Commands>,
}

#[tokio::main]
async fn main() {
    tel_panic::install_panic_hook();
    fabro_telemetry::init_cli();

    let start = std::time::Instant::now();
    let raw_args: Vec<String> = std::env::args().collect();

    let (command_name, result) = Box::pin(main_inner()).await;
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap();

    let is_error = result.is_err();
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
    if let Some(home) = dirs::home_dir() {
        let env_path = home.join(".fabro").join(".env");
        if dotenvy::from_path(&env_path).is_ok() {
            debug!(path = %env_path.display(), "Loaded environment file");
        }
    }

    let Cli { globals, command } = cli;
    let _printer = Printer::from_flags(globals.quiet, globals.verbose);
    let command_name = command.name().to_string();

    let (config_log_level, upgrade_check_enabled) = {
        if let Commands::Server(ServerNamespace {
            command:
                ServerCommand::Start {
                    serve_args: args, ..
                }
                | ServerCommand::Serve {
                    serve_args: args, ..
                },
        }) = command.as_ref()
        {
            match load_server_settings(args.config.as_deref()) {
                Ok(server_settings) => (
                    server_settings.log.as_ref().and_then(|l| l.level.clone()),
                    false,
                ),
                Err(err) => return (command_name, Err(err)),
            }
        } else {
            match user_config::load_user_settings() {
                Ok(cli_settings) => (
                    cli_settings.log.as_ref().and_then(|l| l.level.clone()),
                    cli_settings.upgrade_check_enabled(),
                ),
                Err(err) => return (command_name, Err(err)),
            }
        }
    };

    let log_prefix = if command_name == "server start" || command_name == "server __serve" {
        "server"
    } else {
        "cli"
    };
    if let Err(err) = logging::init_tracing(globals.debug, config_log_level.as_deref(), log_prefix)
    {
        eprintln!("Warning: failed to initialize logging: {err:#}");
    }

    debug!(command = %command_name, "CLI command started");

    let upgrade_handle = if matches!(
        command.as_ref(),
        Commands::RunCmd(RunCommands::Run(_) | RunCommands::Create(_))
            | Commands::Exec(_)
            | Commands::Repo(_)
            | Commands::Install { .. }
    ) {
        commands::upgrade::spawn_upgrade_check(globals.no_upgrade_check, upgrade_check_enabled)
    } else {
        None
    };

    let result = Box::pin(async move {
        match *command {
            Commands::Llm(ns) => commands::llm::dispatch(ns, &globals).await?,
            Commands::Exec(args) => commands::exec::execute(args, &globals).await?,
            Commands::RunCmd(cmd) => Box::pin(commands::run::dispatch(cmd, &globals)).await?,
            Commands::Preflight(args) => commands::preflight::execute(args, &globals).await?,
            Commands::Validate(args) => {
                let styles = Styles::detect_stderr();
                commands::validate::run(&args, &styles, &globals)?;
            }
            Commands::Graph(args) => {
                let styles = Styles::detect_stderr();
                commands::graph::run(&args, &styles, &globals)?;
            }
            Commands::Parse(args) => {
                commands::parse::run(&args, &globals)?;
            }
            Commands::Artifact(ns) => commands::artifact::dispatch(ns, &globals).await?,
            Commands::Store(ns) => commands::store::dispatch(ns, &globals).await?,
            Commands::RunsCmd(cmd) => commands::runs::dispatch(cmd, &globals).await?,
            Commands::Model { command } => commands::model::execute(command, &globals).await?,
            Commands::Server(ns) => {
                commands::server::dispatch(ns.command, &globals).await?;
            }
            Commands::Doctor { verbose, dry_run } => {
                let cli_settings = user_config::load_user_settings()?;
                let verbose = verbose || cli_settings.verbose_enabled();
                let exit_code = commands::doctor::run_doctor(verbose, !dry_run, &globals).await?;
                std::process::exit(exit_code);
            }
            Commands::Discord => {
                if globals.json {
                    shared::print_json_pretty(&serde_json::json!({
                        "url": "https://fabro.sh/discord",
                    }))?;
                } else {
                    open::that("https://fabro.sh/discord")?;
                }
            }
            Commands::Docs => {
                if globals.json {
                    shared::print_json_pretty(&serde_json::json!({
                        "url": "https://docs.fabro.sh/",
                    }))?;
                } else {
                    open::that("https://docs.fabro.sh/")?;
                }
            }
            Commands::Repo(ns) => commands::repo::dispatch(ns, &globals).await?,
            Commands::Install { web_url } => {
                commands::install::run_install(&web_url, &globals).await?;
            }
            Commands::Pr(ns) => Box::pin(commands::pr::dispatch(ns, &globals)).await?,
            Commands::Secret(ns) => commands::secret::dispatch(ns, &globals)?,
            Commands::Settings(args) => commands::config::execute(&args, &globals)?,
            Commands::Workflow(ns) => commands::workflow::dispatch(ns, &globals)?,
            Commands::Skill(ns) => commands::skill::dispatch(ns, &globals)?,
            Commands::Upgrade(args) => {
                commands::upgrade::run_upgrade(args, &globals).await?;
            }
            Commands::Provider(ns) => commands::provider::dispatch(ns, &globals).await?,
            Commands::Sandbox { command } => commands::sandbox::dispatch(command, &globals).await?,
            Commands::System(ns) => commands::system::dispatch(ns, &globals).await?,
            Commands::Completion(args) => {
                globals.require_no_json()?;
                let mut cmd = Cli::command();
                let shell = args.shell;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut buf = Vec::new();
                    clap_complete::generate(shell, &mut cmd, "fabro", &mut buf);
                    buf
                }));
                match result {
                    Ok(buf) => {
                        use std::io::Write;
                        std::io::stdout().write_all(&buf)?;
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
    use super::*;
    use args::{ProviderCommand, ProviderNamespace, StoreCommand, StoreNamespace};
    use clap::Parser;

    #[test]
    fn parse_provider_login_openai() {
        let cli = Cli::try_parse_from(["fabro", "provider", "login", "--provider", "openai"])
            .expect("should parse");
        match *cli.command {
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
        match *cli.command {
            Commands::Provider(ProviderNamespace {
                command: ProviderCommand::Login(args),
            }) => {
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
        match *cli.command {
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
    fn parse_global_storage_dir_after_subcommand() {
        let cli = Cli::try_parse_from([
            "fabro",
            "run",
            "test/simple.fabro",
            "--storage-dir",
            "/tmp/fabro",
        ])
        .expect("should parse");
        assert_eq!(
            cli.globals.storage_dir.as_deref(),
            Some(std::path::Path::new("/tmp/fabro"))
        );
        match *cli.command {
            Commands::RunCmd(RunCommands::Run(args)) => {
                assert_eq!(
                    args.workflow.as_deref(),
                    Some(std::path::Path::new("test/simple.fabro"))
                );
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_server_url_conflicts_with_storage_dir() {
        let result = Cli::try_parse_from([
            "fabro",
            "--storage-dir",
            "/tmp/fabro",
            "--server-url",
            "http://localhost:3000",
            "model",
            "list",
        ]);
        assert!(result.is_err(), "should fail with conflicting global flags");
    }

    #[test]
    fn parse_store_dump_command() {
        let cli = Cli::try_parse_from(["fabro", "store", "dump", "ABC123", "-o", "./out"])
            .expect("should parse");
        match *cli.command {
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
        match *cli.command {
            Commands::RunCmd(RunCommands::Start { run }) => {
                assert_eq!(run, "ABC123");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_attach_command() {
        let cli = Cli::try_parse_from(["fabro", "attach", "ABC123"]).expect("should parse");
        match *cli.command {
            Commands::RunCmd(RunCommands::Attach { run }) => {
                assert_eq!(run, "ABC123");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_sandbox_cp_command() {
        let cli = Cli::try_parse_from(["fabro", "sandbox", "cp", "ABC123:/tmp/file", "./file"])
            .expect("should parse");
        match *cli.command {
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
    fn parse_runner_command() {
        let cli = Cli::try_parse_from([
            "fabro",
            "__runner",
            "--run-id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        ])
        .expect("should parse");
        match *cli.command {
            Commands::RunCmd(RunCommands::Runner { run_id, resume }) => {
                assert_eq!(run_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap());
                assert!(!resume);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_runner_with_resume() {
        let cli = Cli::try_parse_from([
            "fabro",
            "__runner",
            "--run-id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--resume",
        ])
        .expect("should parse");
        match *cli.command {
            Commands::RunCmd(RunCommands::Runner { run_id, resume }) => {
                assert_eq!(run_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap());
                assert!(resume);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_settings_command() {
        let cli = Cli::try_parse_from(["fabro", "settings"]).expect("should parse");
        assert_eq!(cli.command.name(), "settings");
        match *cli.command {
            Commands::Settings(args) => {
                assert!(args.workflow.is_none());
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_settings_with_workflow() {
        let cli = Cli::try_parse_from(["fabro", "settings", "demo"]).expect("should parse");
        match *cli.command {
            Commands::Settings(args) => {
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
