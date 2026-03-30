mod cp;
mod list;

use anyhow::Result;

use crate::args::{AssetCommand, AssetNamespace, GlobalArgs};

pub(crate) fn dispatch(ns: AssetNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        AssetCommand::List(args) => list::list_command(&args, globals),
        AssetCommand::Cp(args) => cp::cp_command(&args, globals),
    }
}
