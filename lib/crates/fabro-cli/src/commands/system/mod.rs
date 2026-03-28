mod df;
mod prune;

use anyhow::Result;

use crate::args::{SystemCommand, SystemNamespace};

pub(crate) use prune::parse_duration;

pub(crate) fn dispatch(ns: SystemNamespace) -> Result<()> {
    match ns.command {
        SystemCommand::Prune(args) => prune::prune_command(&args),
        SystemCommand::Df(args) => df::df_command(&args),
    }
}
