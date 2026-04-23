pub(crate) mod dump;
pub(crate) mod rebuild;
mod run_export;

use anyhow::Result;

use crate::args::{StoreCommand, StoreNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: StoreNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        StoreCommand::Dump(args) => dump::dump_command(&args, base_ctx).await,
    }
}
