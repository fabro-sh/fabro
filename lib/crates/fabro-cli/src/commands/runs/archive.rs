use anyhow::Result;
use futures::FutureExt as _;

use super::{RunBatchAction, run_resolved_run_batch};
use crate::args::{RunsArchiveArgs, RunsUnarchiveArgs};
use crate::command_context::CommandContext;

pub(crate) async fn archive_command(
    args: &RunsArchiveArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    run_resolved_run_batch(
        RunBatchAction {
            past:     "archived",
            json_key: "archived",
        },
        &args.runs,
        &ctx,
        |client, run_id| client.archive_run(run_id).boxed(),
    )
    .await
}

pub(crate) async fn unarchive_command(
    args: &RunsUnarchiveArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    run_resolved_run_batch(
        RunBatchAction {
            past:     "unarchived",
            json_key: "unarchived",
        },
        &args.runs,
        &ctx,
        |client, run_id| client.unarchive_run(run_id).boxed(),
    )
    .await
}
