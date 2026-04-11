mod df;
mod events;
mod info;
mod prune;

use anyhow::Result;
pub(crate) use prune::parse_duration;

use crate::args::{GlobalArgs, SystemCommand, SystemNamespace};

pub(crate) async fn dispatch(ns: SystemNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        SystemCommand::Info(args) => info::info_command(&args, globals).await,
        SystemCommand::Prune(args) => prune::prune_command(&args, globals).await,
        SystemCommand::Df(args) => df::df_command(&args, globals).await,
        SystemCommand::Events(args) => events::events_command(&args, globals).await,
    }
}
