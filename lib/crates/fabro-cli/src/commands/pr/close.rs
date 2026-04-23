use anyhow::Result;
use tracing::info;

use crate::args::PrCloseArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn close_command(args: PrCloseArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    let response = client.close_run_pull_request(&run_id).await?;

    info!(number = response.number, "Closed pull request");
    if ctx.json_output() {
        print_json_pretty(&response)?;
    } else {
        fabro_util::printout!(
            ctx.printer(),
            "Closed #{} ({})",
            response.number,
            response.html_url
        );
    }

    Ok(())
}
