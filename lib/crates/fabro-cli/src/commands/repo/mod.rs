pub(crate) mod deinit;
pub(crate) mod init;

use anyhow::Result;
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, RepoCommand, RepoNamespace};
use crate::shared::print_json_pretty;

pub(crate) async fn dispatch(
    ns: RepoNamespace,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        RepoCommand::Init(args) => {
            let created = init::run_init(&args, globals, printer).await?;
            if globals.json {
                print_json_pretty(&serde_json::json!({ "created": created }))?;
            }
            Ok(())
        }
        RepoCommand::Deinit => {
            let removed = deinit::run_deinit(globals, printer)?;
            if globals.json {
                print_json_pretty(&serde_json::json!({ "removed": removed }))?;
            }
            Ok(())
        }
    }
}
