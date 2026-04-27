use anyhow::Result;
use clap::Args;

use super::{PlannedCommand, spa_refresh};

#[derive(Debug, Args)]
pub(crate) struct BuildArgs {
    /// Arguments forwarded to `cargo build`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cargo_args: Vec<String>,
}

pub(crate) fn build(args: BuildArgs) -> Result<()> {
    let root = super::workspace_root();
    spa_refresh::spa_refresh_root(&root)?;

    let mut command = PlannedCommand::new("cargo").arg("build");
    for arg in args.cargo_args {
        command = command.arg(arg);
    }

    super::run_command(&root, &command)
}
