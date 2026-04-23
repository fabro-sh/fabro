use anyhow::Result;
use tracing::info;

use crate::args::PrMergeArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn merge_command(args: PrMergeArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    let method: fabro_github::AutoMergeMethod = args.method.into();
    let response = client.merge_run_pull_request(&run_id, method).await?;

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
