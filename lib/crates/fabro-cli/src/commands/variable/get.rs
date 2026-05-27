use anyhow::Result;

use crate::args::VariableGetArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn get_command(args: &VariableGetArgs, ctx: &CommandContext) -> Result<()> {
    let client = ctx.server().await?;
    let variable = client.get_variable(&args.name).await?;
    if ctx.json_output() {
        print_json_pretty(&variable)?;
    } else {
        fabro_util::printout!(ctx.printer(), "{}", variable.value);
    }
    Ok(())
}
