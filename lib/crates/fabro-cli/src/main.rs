#![allow(clippy::print_stdout, clippy::print_stderr, clippy::exit)]

mod args;
mod commands;
mod logging;
mod manifest_builder;
mod server_client;
mod server_runs;
mod shared;
#[cfg(feature = "sleep_inhibitor")]
mod sleep_inhibitor;
mod user_config;

use anyhow::Result;
use args::{Commands, GlobalArgs, LONG_VERSION, RunCommands, ServerCommand, ServerNamespace};
use clap::{CommandFactory, FromArgMatches, Parser, error::ErrorKind, parser::ValueSource};
use fabro_config::server::load_server_settings;
use fabro_telemetry::{git, panic as tel_panic, sanitize, sender};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use rustls::crypto::ring::default_provider;
use std::ffi::OsString;
use tracing::debug;

#[derive(Parser)]
#[command(name = "fabro", version, long_version = LONG_VERSION)]
struct Cli {
    #[command(flatten)]
    globals: GlobalArgs,

    #[command(subcommand)]
    command: Box<Commands>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ServerConnectionValueSources {
    storage_dir: Option<ValueSource>,
    server: Option<ValueSource>,
}

impl ServerConnectionValueSources {
    fn command_line_conflict(self) -> bool {
        self.storage_dir == Some(ValueSource::CommandLine)
            && self.server == Some(ValueSource::CommandLine)
    }

    fn storage_dir_is_explicit(self) -> bool {
        self.storage_dir == Some(ValueSource::CommandLine)
    }
}

impl Cli {
    fn parse() -> Self {
        Self::try_parse_from(std::env::args_os()).unwrap_or_else(|err| err.exit())
    }

    fn try_parse_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        let mut command = Self::command();
        let mut matches = command.try_get_matches_from_mut(args)?;
        let sources = server_connection_value_sources(&matches);
        if sources.command_line_conflict() {
            return Err(command.error(
                ErrorKind::ArgumentConflict,
                "the argument '--server <SERVER>' cannot be used with '--storage-dir <STORAGE_DIR>'",
            ));
        }

        let mut cli = <Self as FromArgMatches>::from_arg_matches_mut(&mut matches)?;
        cli.apply_server_connection_value_sources(sources);
        Ok(cli)
    }

    fn apply_server_connection_value_sources(&mut self, sources: ServerConnectionValueSources) {
        self.command.apply_server_connection_value_sources(sources);
    }
}

impl Commands {
    fn apply_server_connection_value_sources(&mut self, sources: ServerConnectionValueSources) {
        match self {
            Self::RunCmd(RunCommands::Run(args) | RunCommands::Create(args)) => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Preflight(args) => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Validate(args) => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Graph(args) => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Model {
                command: Some(args::ModelsCommand::List(args)),
            } => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Model {
                command: Some(args::ModelsCommand::Test(args)),
            } => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Doctor(args) => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Repo(args::RepoNamespace {
                command: args::RepoCommand::Init(args),
            }) => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Provider(args::ProviderNamespace {
                command: args::ProviderCommand::Login(args),
            }) => {
                args.target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            Self::Secret(args::SecretNamespace { target, .. }) => {
                target.storage_dir_explicit = sources.storage_dir_is_explicit();
            }
            _ => {}
        }
    }
}

fn server_connection_value_sources(matches: &clap::ArgMatches) -> ServerConnectionValueSources {
    let sources = ServerConnectionValueSources {
        storage_dir: matches
            .try_contains_id("storage_dir")
            .ok()
            .and_then(|present| present.then(|| matches.value_source("storage_dir")))
            .flatten(),
        server: matches
            .try_contains_id("server")
            .ok()
            .and_then(|present| present.then(|| matches.value_source("server")))
            .flatten(),
    };
    if sources.storage_dir.is_some() || sources.server.is_some() {
        return sources;
    }

    matches
        .subcommand()
        .map(|(_, subcommand_matches)| server_connection_value_sources(subcommand_matches))
        .unwrap_or_default()
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

    let Cli { globals, command } = cli;
    let _printer = Printer::from_flags(globals.quiet, globals.verbose);
    let command_name = command.name().to_string();

    let (config_log_level, upgrade_check_enabled) = {
        if let Commands::Server(ServerNamespace {
            command:
                ServerCommand::Start(args::ServerStartArgs {
                    serve_args: args, ..
                })
                | ServerCommand::Serve(args::ServerServeArgs {
                    serve_args: args, ..
                }),
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
            | Commands::Install(_)
    ) {
        commands::upgrade::spawn_upgrade_check(globals.no_upgrade_check, upgrade_check_enabled)
    } else {
        None
    };

    let result = Box::pin(async move {
        match *command {
            Commands::Exec(args) => commands::exec::execute(args, &globals).await?,
            Commands::RunCmd(cmd) => Box::pin(commands::run::dispatch(cmd, &globals)).await?,
            Commands::Preflight(args) => commands::preflight::execute(args, &globals).await?,
            Commands::Validate(args) => {
                let styles = Styles::detect_stderr();
                commands::validate::run(&args, &styles, &globals).await?;
            }
            Commands::Graph(args) => {
                let styles = Styles::detect_stderr();
                commands::graph::run(&args, &styles, &globals).await?;
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
            Commands::Doctor(args) => {
                let cli_settings = user_config::load_user_settings()?;
                let verbose = args.verbose || cli_settings.verbose_enabled();
                let exit_code = commands::doctor::run_doctor(&args, verbose, &globals).await?;
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
            Commands::Install(args) => {
                commands::install::run_install(&args, &globals).await?;
            }
            Commands::Pr(ns) => Box::pin(commands::pr::dispatch(ns, &globals)).await?,
            Commands::Secret(ns) => commands::secret::dispatch(ns, &globals).await?,
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
            #[cfg(debug_assertions)]
            Commands::TestPanic { message } => {
                let event = tel_panic::build_event(&message);
                let json = serde_json::to_string_pretty(&event)?;
                println!("{json}");
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
    use args::{
        Commands, ModelsCommand, ProviderCommand, ProviderNamespace, StoreCommand, StoreNamespace,
    };

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
    fn parse_run_storage_dir_after_subcommand() {
        let cli = Cli::try_parse_from([
            "fabro",
            "run",
            "test/simple.fabro",
            "--storage-dir",
            "/tmp/fabro",
        ])
        .expect("should parse");
        match *cli.command {
            Commands::RunCmd(RunCommands::Run(args)) => {
                assert_eq!(
                    args.target.storage_dir(),
                    Some(std::path::Path::new("/tmp/fabro"))
                );
                assert_eq!(
                    args.workflow.as_deref(),
                    Some(std::path::Path::new("test/simple.fabro"))
                );
            }
            _ => panic!("unexpected command variant"),
        }
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
        match *cli.command {
            Commands::Model {
                command: Some(ModelsCommand::List(args)),
            } => assert_eq!(args.target.server(), Some("http://localhost:3000/api/v1")),
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
        match *cli.command {
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
            Commands::RunCmd(RunCommands::Start(args)) => {
                assert_eq!(args.run, "ABC123");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_attach_command() {
        let cli = Cli::try_parse_from(["fabro", "attach", "ABC123"]).expect("should parse");
        match *cli.command {
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
            Commands::RunCmd(RunCommands::Runner(args)) => {
                assert_eq!(args.run_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap());
                assert!(!args.resume);
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
            Commands::RunCmd(RunCommands::Runner(args)) => {
                assert_eq!(args.run_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap());
                assert!(args.resume);
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
