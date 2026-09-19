use anyhow::Result;

use super::checkpoints::short_id;
use crate::args::RetryArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

/// Retry a finished run from its last checkpoint in a new run, which starts
/// at once.
pub(crate) async fn run(args: &RetryArgs, base_ctx: &CommandContext) -> Result<()> {
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.id;
    let new_run = client.retry_run(&run_id).await?;

    if ctx.json_output() {
        print_json_pretty(&serde_json::json!({
            "source_run_id": run_id,
            "run_id": new_run.id,
        }))?;
    } else {
        fabro_util::printerr!(
            printer,
            "Retrying {} as {}",
            short_id(&run_id.to_string()),
            short_id(&new_run.id.to_string())
        );
        fabro_util::printout!(printer, "{}", new_run.id);
    }
    Ok(())
}
