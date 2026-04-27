use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand};

use super::docs_cli_reference::docs_cli_reference_root;
use super::docs_options_reference::docs_options_reference_root;
use super::workspace_root;

#[derive(Debug, Args)]
pub(crate) struct DocsArgs {
    #[command(subcommand)]
    command: Option<DocsCommand>,
}

#[derive(Debug, Subcommand)]
enum DocsCommand {
    /// Regenerate generated reference documentation.
    Refresh(DocsCommandArgs),
    /// Verify generated reference documentation is up to date.
    Check(DocsCommandArgs),
}

#[derive(Debug, Args)]
struct DocsCommandArgs {
    /// Workspace root containing docs/public/reference.
    #[arg(long, hide = true)]
    root: Option<PathBuf>,
}

pub(crate) fn docs(args: DocsArgs) -> Result<()> {
    match args.command {
        Some(DocsCommand::Refresh(args)) => refresh_docs(&args.root.unwrap_or_else(workspace_root)),
        Some(DocsCommand::Check(args)) => check_docs(&args.root.unwrap_or_else(workspace_root)),
        None => print_docs_help(),
    }
}

fn refresh_docs(root: &Path) -> Result<()> {
    docs_cli_reference_root(root, false)?;
    docs_options_reference_root(root, false)
}

fn check_docs(root: &Path) -> Result<()> {
    docs_cli_reference_root(root, true)?;
    docs_options_reference_root(root, true)
}

#[expect(
    clippy::print_stdout,
    reason = "dev docs command prints group help for non-mutating discovery"
)]
fn print_docs_help() -> Result<()> {
    let mut command = DocsCommand::augment_subcommands(
        clap::Command::new("docs")
            .about("Manage generated reference documentation")
            .override_usage("fabro-dev docs <COMMAND>"),
    );
    command.print_help()?;
    println!();
    Ok(())
}
