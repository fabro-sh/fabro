mod create;
mod list;

use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_util::printer::Printer;

use crate::args::{WorkflowCommand, WorkflowNamespace};

pub(crate) fn dispatch(ns: WorkflowNamespace, cli: &CliSettings, printer: Printer) -> Result<()> {
    match ns.command {
        WorkflowCommand::List(args) => list::list_command(&args, cli, printer),
        WorkflowCommand::Create(args) => create::create_command(&args, cli, printer),
    }
}
