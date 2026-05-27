use anyhow::Result;

use crate::args::VariableRmArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn rm_command(args: &VariableRmArgs, ctx: &CommandContext) -> Result<()> {
    let client = ctx.server().await?;
    client.delete_variable(&args.name).await?;
    if ctx.json_output() {
        print_json_pretty(&serde_json::json!({ "name": args.name }))?;
    } else {
        fabro_util::printerr!(ctx.printer(), "Removed {}", args.name);
    }
    Ok(())
}
