mod get;
mod list;
mod rm;
mod set;

use anyhow::Result;

use crate::args::{GlobalArgs, SecretCommand, SecretNamespace};

pub(crate) fn dispatch(ns: SecretNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        SecretCommand::Get(args) => get::get_command(&args, globals),
        SecretCommand::List(args) => list::list_command(&args, globals),
        SecretCommand::Rm(args) => rm::rm_command(&args, globals),
        SecretCommand::Set(args) => set::set_command(&args, globals),
    }
}
