pub(crate) mod deinit;
pub(crate) mod init;

use anyhow::Result;

use crate::args::{RepoCommand, RepoNamespace};
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(crate) async fn dispatch(ns: RepoNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        RepoCommand::Init(args) => {
            let created = init::run_init(&args, base_ctx).await?;
            if base_ctx.json_output() {
                print_json_pretty(&serde_json::json!({ "created": created }))?;
            }
            Ok(())
        }
        RepoCommand::Deinit => {
            let removed = deinit::run_deinit(base_ctx)?;
            if base_ctx.json_output() {
                print_json_pretty(&serde_json::json!({ "removed": removed }))?;
            }
            Ok(())
        }
    }
}
