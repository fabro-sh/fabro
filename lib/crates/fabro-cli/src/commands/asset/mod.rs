mod cp;
mod list;

use anyhow::Result;

use crate::args::{AssetCommand, AssetNamespace};

pub fn dispatch(ns: AssetNamespace) -> Result<()> {
    match ns.command {
        AssetCommand::List(args) => list::list_command(&args),
        AssetCommand::Cp(args) => cp::cp_command(&args),
    }
}
