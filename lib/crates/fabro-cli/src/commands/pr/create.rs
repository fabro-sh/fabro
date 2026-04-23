use anyhow::Result;
use tracing::info;

use crate::args::PrCreateArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn create_command(args: PrCreateArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    let record = client
        .create_run_pull_request(&run_id, args.force, args.model)
        .await?;

    info!(
        number = record.number,
        owner = %record.owner,
        repo = %record.repo,
        "Created pull request"
    );

    if ctx.json_output() {
        print_json_pretty(&record)?;
    } else {
        fabro_util::printout!(ctx.printer(), "{}", record.html_url);
    }

    Ok(())
}
