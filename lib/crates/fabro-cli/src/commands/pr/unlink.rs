use anyhow::Result;
use tracing::info;

use crate::args::PrUnlinkArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn unlink_command(args: PrUnlinkArgs, base_ctx: &CommandContext) -> Result<()> {
    let (ctx, client, run_id) =
        super::resolve_run_selector(base_ctx, &args.server, &args.run_id).await?;
    let record = client.unlink_run_pull_request(&run_id).await?;

    info!(
        pr_url = %record.html_url(),
        number = record.number,
        "Unlinked pull request"
    );

    if ctx.json_output() {
        print_json_pretty(&record)?;
    } else {
        fabro_util::printout!(
            ctx.printer(),
            "Unlinked pull request: {}",
            record.html_url()
        );
    }

    Ok(())
}
