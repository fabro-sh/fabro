use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;

#[derive(Debug, Parser)]
#[command(
    name = "fabro-dev",
    version,
    about = "Internal development tooling for Fabro"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check source boundary rules.
    CheckBoundary(commands::CheckBoundaryArgs),
    /// Build Fabro Docker images with the release pipeline layout.
    DockerBuild(commands::DockerBuildArgs),
    /// Generate docs/reference/cli.mdx from the Fabro clap command tree.
    GenerateCliReference(commands::GenerateCliReferenceArgs),
    /// Generate docs/reference/user-configuration.mdx from settings metadata.
    GenerateOptionsReference(commands::GenerateOptionsReferenceArgs),
    /// Run Fabro release automation.
    Release(commands::ReleaseArgs),
    /// Refresh the embedded Fabro web SPA bundle.
    RefreshSpa(commands::RefreshSpaArgs),
    /// Check embedded Fabro web SPA asset budgets.
    CheckSpaBudgets(commands::CheckSpaBudgetsArgs),
}

impl Command {
    fn run(self) -> Result<()> {
        match self {
            Self::CheckBoundary(args) => commands::check_boundary(args),
            Self::DockerBuild(args) => commands::docker_build(args),
            Self::GenerateCliReference(args) => commands::generate_cli_reference(args),
            Self::GenerateOptionsReference(args) => commands::generate_options_reference(args),
            Self::Release(args) => commands::release(args),
            Self::RefreshSpa(args) => commands::refresh_spa(args),
            Self::CheckSpaBudgets(args) => commands::check_spa_budgets(args),
        }
    }
}

fn main() -> ExitCode {
    install_tracing();

    match Cli::parse().command.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_error(&error);
            ExitCode::FAILURE
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev CLI installs a process-global stderr tracing sink before command dispatch"
)]
fn install_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

#[expect(
    clippy::print_stderr,
    reason = "dev CLI reports final command errors to stderr"
)]
fn report_error(error: &anyhow::Error) {
    eprintln!("fabro-dev failed");
    for cause in error.chain() {
        eprintln!("  caused by: {cause}");
    }
}
