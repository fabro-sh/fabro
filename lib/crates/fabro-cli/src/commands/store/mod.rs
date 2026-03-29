mod dump;

use anyhow::Result;

use crate::args::{StoreCommand, StoreNamespace};

pub(crate) async fn dispatch(ns: StoreNamespace) -> Result<()> {
    match ns.command {
        StoreCommand::Dump(args) => dump::dump_command(&args).await,
    }
}
