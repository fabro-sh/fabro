mod args;
mod cli_config;
mod commands;
mod logging;
mod shared;

use anyhow::Result;
use args::*;
use clap::Parser;
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

    let Cli { globals, command } = cli;
    let command_name = command.name().to_string();

    let (config_log_level, upgrade_check_enabled) = {
        #[cfg(feature = "server")]
        {
            if let Commands::Serve(args) = command.as_ref() {
                match fabro_config::server::load_server_config(args.config.as_deref()) {
                    Ok(server_config) => (
                        server_config.log.as_ref().and_then(|l| l.level.clone()),
                        false,
                    ),
                    Err(err) => return (command_name, Err(err)),
                }
            } else {
                match fabro_config::cli::load_cli_config(None) {
                    Ok(cli_config) => (
                        cli_config.log.as_ref().and_then(|l| l.level.clone()),
                        cli_config.upgrade_check_enabled(),
                    ),
                    Err(err) => return (command_name, Err(err)),
                }
            }
        }
        #[cfg(not(feature = "server"))]
        {
            match fabro_config::cli::load_cli_config(None) {
                Ok(cli_config) => (
                    cli_config.log.as_ref().and_then(|l| l.level.clone()),
                    cli_config.upgrade_check_enabled(),
                ),
                Err(err) => return (command_name, Err(err)),
            }
        }
    };

    let log_prefix = if command_name == "serve" {
        "serve"
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
            | Commands::Init
            | Commands::Install { .. }
    ) {
        commands::upgrade::spawn_upgrade_check(globals.no_upgrade_check, upgrade_check_enabled)
    } else {
        None
    };

    let result = async move {
        match *command {
            Commands::Llm(ns) => commands::llm::dispatch(ns, &globals).await?,
            Commands::Exec(args) => commands::exec::execute(args, &globals).await?,
            Commands::RunCmd(cmd) => commands::run::dispatch(cmd, &globals).await?,
            Commands::Preflight(args) => commands::preflight::execute(args).await?,
            Commands::Validate(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                commands::validate::run(&args, &styles)?;
            }
            Commands::Graph(args) => {
                let styles = fabro_util::terminal::Styles::detect_stderr();
                commands::graph::run(&args, &styles)?;
            }
            Commands::Parse(args) => {
                commands::parse::run(&args)?;
            }
            Commands::Asset(ns) => commands::asset::dispatch(ns)?,
            Commands::RunsCmd(cmd) => commands::runs::dispatch(cmd).await?,
            Commands::Model { command } => commands::model::execute(command, &globals).await?,
            #[cfg(feature = "server")]
            Commands::Serve(args) => {
                let styles: &'static fabro_util::terminal::Styles =
                    Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
                fabro_api::serve::serve_command(args, styles).await?;
            }
            Commands::Doctor { verbose, dry_run } => {
                let cli_config = cli_config::load_cli_config(None)?;
                let verbose = verbose || cli_config.verbose_enabled();
                let exit_code = commands::doctor::run_doctor(verbose, !dry_run).await;
                std::process::exit(exit_code);
            }
            Commands::Discord => {
                open::that("https://fabro.sh/discord")?;
            }
            Commands::Docs => {
                open::that("https://docs.fabro.sh/")?;
            }
            Commands::Repo(ns) => commands::repo::dispatch(ns).await?,
            Commands::Init => {
                eprintln!(
                    "{} `fabro init` is deprecated, use `fabro repo init` instead",
                    console::Style::new().yellow().apply_to("warning:")
                );
                commands::repo::init::run_init().await?;
            }
            Commands::Install { web_url } => {
                commands::install::run_install(&web_url).await?;
            }
            Commands::Pr(ns) => commands::pr::dispatch(ns).await?,
            Commands::Secret(ns) => commands::secret::dispatch(ns)?,
            Commands::Config(ns) => commands::config::dispatch(ns)?,
            Commands::Workflow(ns) => commands::workflow::dispatch(ns)?,
            Commands::Skill(ns) => commands::skill::dispatch(ns)?,
            Commands::Upgrade(args) => {
                commands::upgrade::run_upgrade(args).await?;
            }
            Commands::Provider(ns) => commands::provider::dispatch(ns).await?,
            Commands::System(ns) => commands::system::dispatch(ns)?,
            Commands::SendAnalytics { path } => {
                let result = fabro_telemetry::sender::upload(&path).await;
                let _ = std::fs::remove_file(&path);
                result?;
            }
            Commands::SendPanic { path } => {
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
    fn parse_cp_command() {
        let cli = Cli::try_parse_from(["fabro", "cp", "ABC123:/tmp/file", "./file"])
            .expect("should parse");
        match *cli.command {
            Commands::RunCmd(RunCommands::Cp(args)) => {
                assert_eq!(args.src, "ABC123:/tmp/file");
                assert_eq!(args.dst, "./file");
                assert!(!args.recursive);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_detached_command() {
        let cli = Cli::try_parse_from([
            "fabro",
            "__detached",
            "--storage-dir",
            "/tmp/fabro",
            "--run-id",
            "01ABC",
        ])
        .expect("should parse");
        match *cli.command {
            Commands::RunCmd(RunCommands::Detached {
                storage_dir,
                run_id,
                resume,
            }) => {
                assert_eq!(storage_dir, std::path::PathBuf::from("/tmp/fabro"));
                assert_eq!(run_id, "01ABC");
                assert!(!resume);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_detached_with_resume() {
        let cli = Cli::try_parse_from([
            "fabro",
            "__detached",
            "--storage-dir",
            "/tmp/fabro",
            "--run-id",
            "01ABC",
            "--resume",
        ])
        .expect("should parse");
        match *cli.command {
            Commands::RunCmd(RunCommands::Detached {
                storage_dir,
                run_id,
                resume,
            }) => {
                assert_eq!(storage_dir, std::path::PathBuf::from("/tmp/fabro"));
                assert_eq!(run_id, "01ABC");
                assert!(resume);
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_config_show_command() {
        let cli = Cli::try_parse_from(["fabro", "config", "show"]).expect("should parse");
        assert_eq!(cli.command.name(), "config show");
        match *cli.command {
            Commands::Config(ConfigNamespace {
                command: ConfigCommand::Show(args),
            }) => {
                assert!(args.workflow.is_none());
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_config_show_with_workflow() {
        let cli = Cli::try_parse_from(["fabro", "config", "show", "demo"]).expect("should parse");
        match *cli.command {
            Commands::Config(ConfigNamespace {
                command: ConfigCommand::Show(args),
            }) => {
                assert_eq!(args.workflow, Some(std::path::PathBuf::from("demo")));
            }
            _ => panic!("unexpected command variant"),
        }
    }
}
