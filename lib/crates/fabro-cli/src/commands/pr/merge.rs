use anyhow::Result;
use tracing::info;

use crate::args::PrMergeArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn merge_command(args: PrMergeArgs, base_ctx: &CommandContext) -> Result<()> {
    let (ctx, client, run_id) =
        super::resolve_run_for_pr(base_ctx, &args.server, &args.run_id).await?;
    let response = client.merge_run_pull_request(&run_id, args.method).await?;

    info!(
        number = response.number,
        method = %response.method,
        "Merged pull request"
    );
    if ctx.json_output() {
        print_json_pretty(&response)?;
    } else {
        fabro_util::printout!(
            ctx.printer(),
            "Merged #{} ({})",
            response.number,
            response.html_url
        );
    }

    Ok(())
}
