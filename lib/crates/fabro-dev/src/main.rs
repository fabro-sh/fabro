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
    /// Run the test suite N times and capture per-test timing to CSV.
    BenchTests(commands::BenchTestsArgs),
    /// Refresh embedded SPA assets and run cargo build.
    Build(commands::BuildArgs),
    /// Build Fabro Docker images with the release pipeline layout.
    DockerBuild(commands::DockerBuildArgs),
    /// Manage generated reference documentation.
    Docs(commands::DocsArgs),
    /// Run Fabro release automation.
    Release(commands::ReleaseArgs),
    /// Manage embedded Fabro web SPA assets.
    Spa(commands::SpaArgs),
}

impl Command {
    fn run(self) -> Result<()> {
        match self {
            Self::BenchTests(args) => commands::bench_tests(args),
            Self::Build(args) => commands::build(args),
            Self::DockerBuild(args) => commands::docker_build(args),
            Self::Docs(args) => commands::docs(args),
            Self::Release(args) => commands::release(args),
            Self::Spa(args) => commands::spa(args),
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
