mod cp;
mod list;

use anyhow::Result;

use crate::args::{ArtifactCommand, ArtifactNamespace, GlobalArgs};

pub(crate) async fn dispatch(ns: ArtifactNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        ArtifactCommand::List(args) => list::list_command(&args, globals).await,
        ArtifactCommand::Cp(args) => cp::cp_command(&args, globals).await,
    }
}
