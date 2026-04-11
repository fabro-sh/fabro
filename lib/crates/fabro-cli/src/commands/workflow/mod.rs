mod create;
mod list;

use anyhow::Result;
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, WorkflowCommand, WorkflowNamespace};

pub(crate) fn dispatch(
    ns: WorkflowNamespace,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        WorkflowCommand::List(args) => list::list_command(&args, globals, printer),
        WorkflowCommand::Create(args) => create::create_command(&args, globals, printer),
    }
}
