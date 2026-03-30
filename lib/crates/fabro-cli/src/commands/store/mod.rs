mod dump;

use anyhow::Result;

use crate::args::{GlobalArgs, StoreCommand, StoreNamespace};

pub(crate) async fn dispatch(ns: StoreNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        StoreCommand::Dump(args) => dump::dump_command(&args, globals).await,
    }
}
