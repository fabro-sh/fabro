mod get;
mod list;
mod rm;
mod set;

use anyhow::Result;

use crate::args::{SecretCommand, SecretNamespace};

pub fn dispatch(ns: SecretNamespace) -> Result<()> {
    match ns.command {
        SecretCommand::Get(args) => get::get_command(&args),
        SecretCommand::List(args) => list::list_command(&args),
        SecretCommand::Rm(args) => rm::rm_command(&args),
        SecretCommand::Set(args) => set::set_command(&args),
    }
}
