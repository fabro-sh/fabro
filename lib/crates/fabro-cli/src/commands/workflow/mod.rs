mod create;
mod list;

use anyhow::Result;

use crate::args::{GlobalArgs, WorkflowCommand, WorkflowNamespace};

pub(crate) fn dispatch(ns: WorkflowNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        WorkflowCommand::List(args) => list::list_command(&args, globals),
        WorkflowCommand::Create(args) => create::create_command(&args, globals),
    }
}
