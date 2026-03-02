mod logging;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::debug;

#[derive(Parser)]
#[command(name = "arc", version)]
struct Cli {
    /// Skip loading .env file
    #[arg(long, global = true)]
    no_dotenv: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// LLM prompt and model operations
    Llm {
        #[command(subcommand)]
        command: LlmCommand,
    },
    /// Run an agentic coding session
    Agent(arc_agent::cli::AgentArgs),
    /// Launch a pipeline
    Run(arc_workflows::cli::RunArgs),
    /// Validate a pipeline
    Validate(arc_workflows::cli::ValidateArgs),
    /// List and manage pipeline runs
    Runs(arc_workflows::cli::runs::RunsArgs),
    /// Start the HTTP API server
    Serve(arc_api::serve::ServeArgs),
}

#[derive(Subcommand)]
enum LlmCommand {
    /// Execute a prompt
    Prompt(arc_llm::cli::PromptArgs),
    /// Manage models
    Models {
        #[command(subcommand)]
        command: Option<arc_llm::cli::ModelsCommand>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if !cli.no_dotenv {
        dotenvy::dotenv().ok();
    }

    if let Err(err) = logging::init_tracing() {
        eprintln!("Warning: failed to initialize logging: {err:#}");
    }

    let command_name = match &cli.command {
        Command::Llm { .. } => "llm",
        Command::Agent(_) => "agent",
        Command::Run(_) => "run",
        Command::Validate(_) => "validate",
        Command::Runs(_) => "runs",
        Command::Serve(_) => "serve",
    };
    debug!(command = %command_name, "CLI command started");

    match cli.command {
        Command::Llm { command } => match command {
            LlmCommand::Prompt(args) => arc_llm::cli::run_prompt(args).await?,
            LlmCommand::Models { command } => arc_llm::cli::run_models(command).await?,
        },
        Command::Agent(args) => arc_agent::cli::run_with_args(args).await?,
        Command::Run(args) => {
            let styles: &'static arc_util::terminal::Styles =
                Box::leak(Box::new(arc_util::terminal::Styles::detect_stderr()));
            arc_workflows::cli::run::run_command(args, styles).await?;
        }
        Command::Validate(args) => {
            let styles = arc_util::terminal::Styles::detect_stderr();
            arc_workflows::cli::validate::validate_command(&args, &styles)?;
        }
        Command::Runs(args) => {
            arc_workflows::cli::runs::runs_command(args)?;
        }
        Command::Serve(args) => {
            let styles: &'static arc_util::terminal::Styles =
                Box::leak(Box::new(arc_util::terminal::Styles::detect_stderr()));
            arc_api::serve::serve_command(args, styles).await?;
        }
    }

    Ok(())
}
