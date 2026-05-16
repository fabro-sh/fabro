use anyhow::Result;
use fabro_types::PullRequestRecord;
use tracing::info;

use crate::args::PrLinkArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn link_command(args: PrLinkArgs, base_ctx: &CommandContext) -> Result<()> {
    let (ctx, client, run_id) =
        super::resolve_run_for_pr(base_ctx, &args.server, &args.run_id).await?;
    let record = client.link_run_pull_request(&run_id, args.url).await?;

    info!(
        pr_url = %record.html_url(),
        number = record.number,
        "Linked pull request"
    );

    if ctx.json_output() {
        print_json_pretty(&record)?;
    } else {
        fabro_util::printout!(
            ctx.printer(),
            "Linked pull request: {} ({})",
            record.html_url(),
            record_label(&record)
        );
    }

    Ok(())
}

fn record_label(record: &PullRequestRecord) -> String {
    format!("github #{}", record.number)
}
