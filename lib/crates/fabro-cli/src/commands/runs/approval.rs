use anyhow::Result;
use futures::FutureExt as _;

use super::{RunBatchAction, run_resolved_run_batch};
use crate::args::{RunsApproveArgs, RunsDenyArgs};
use crate::command_context::CommandContext;

pub(crate) async fn approve_command(
    args: &RunsApproveArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    run_resolved_run_batch(
        RunBatchAction {
            past:     "approved",
            json_key: "approved",
        },
        &args.runs,
        &ctx,
        |client, run_id| client.approve_run(run_id).boxed(),
    )
    .await
}

pub(crate) async fn deny_command(args: &RunsDenyArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let reason = args.reason.clone();
    run_resolved_run_batch(
        RunBatchAction {
            past:     "denied",
            json_key: "denied",
        },
        &args.runs,
        &ctx,
        move |client, run_id| client.deny_run(run_id, reason.clone()).boxed(),
    )
    .await
}
