mod create;
mod list;

use anyhow::Result;

use crate::args::{WorkflowCommand, WorkflowNamespace};

pub(crate) fn dispatch(ns: WorkflowNamespace) -> Result<()> {
    match ns.command {
        WorkflowCommand::List(args) => list::list_command(&args),
        WorkflowCommand::Create(args) => create::create_command(&args),
    }
}
