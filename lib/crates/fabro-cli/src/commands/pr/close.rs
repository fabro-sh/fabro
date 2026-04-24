use anyhow::Result;
use tracing::info;

use crate::args::PrCloseArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn close_command(args: PrCloseArgs, base_ctx: &CommandContext) -> Result<()> {
    let (ctx, client, run_id) =
        super::resolve_run_for_pr(base_ctx, &args.server, &args.run_id).await?;
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
