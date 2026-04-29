use anyhow::Result;
use clap::{Args, Subcommand};

use super::spa_check::{SpaCheckArgs, spa_check};
use super::spa_refresh::{SpaRefreshArgs, spa_refresh};

#[derive(Debug, Args)]
pub(crate) struct SpaArgs {
    #[command(subcommand)]
    command: Option<SpaCommand>,
}

#[derive(Debug, Subcommand)]
enum SpaCommand {
    /// Rebuild and refresh the embedded Fabro web SPA bundle.
    Refresh(SpaRefreshArgs),
    /// Verify embedded Fabro web SPA assets are current and within budget.
    Check(SpaCheckArgs),
}

pub(crate) fn spa(args: SpaArgs) -> Result<()> {
    match args.command {
        Some(SpaCommand::Refresh(args)) => spa_refresh(args),
        Some(SpaCommand::Check(args)) => spa_check(args),
        None => print_spa_help(),
    }
}

#[expect(
    clippy::print_stdout,
    reason = "dev spa command prints group help for non-mutating discovery"
)]
fn print_spa_help() -> Result<()> {
    let mut command = SpaCommand::augment_subcommands(
        clap::Command::new("spa")
            .about("Manage embedded Fabro web SPA assets")
            .override_usage("fabro-dev spa <COMMAND>"),
    );
    command.print_help()?;
    println!();
    Ok(())
}
