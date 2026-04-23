mod cp;
mod list;

use anyhow::{Context, Result};
use fabro_types::{RunId, StageId};

use crate::args::{ArtifactCommand, ArtifactNamespace, ServerTargetArgs};
use crate::command_context::CommandContext;
use crate::server_client::Client;

#[derive(Clone, Debug, serde::Serialize)]
pub(super) struct ArtifactEntry {
    #[serde(skip_serializing)]
    pub(super) stage_id:      StageId,
    pub(super) node_slug:     String,
    pub(super) retry:         u32,
    pub(super) relative_path: String,
    pub(super) size:          u64,
}

pub(super) async fn resolve_artifacts(
    base_ctx: &CommandContext,
    server: &ServerTargetArgs,
    run_selector: &str,
    node: Option<&str>,
    retry: Option<u32>,
) -> Result<(RunId, Client, Vec<ArtifactEntry>)> {
    let ctx = base_ctx.with_target(server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(run_selector).await?.run_id;
    let mut entries = Vec::new();
    for entry in client.list_run_artifacts(&run_id).await? {
        if node.is_some_and(|value| entry.node_slug != value) {
            continue;
        }
        let entry_retry = u32::try_from(entry.retry)
            .context("server returned invalid negative artifact retry")?;
        if retry.is_some_and(|value| entry_retry != value) {
            continue;
        }
        let size =
            u64::try_from(entry.size).context("server returned invalid negative artifact size")?;
        entries.push(ArtifactEntry {
            stage_id: entry.stage_id.parse()?,
            node_slug: entry.node_slug,
            retry: entry_retry,
            relative_path: entry.relative_path,
            size,
        });
    }

    entries.sort_by(|a, b| {
        a.stage_id
            .cmp(&b.stage_id)
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });

    Ok((run_id, client.clone_for_reuse(), entries))
}

pub(crate) async fn dispatch(ns: ArtifactNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        ArtifactCommand::List(args) => list::list_command(&args, base_ctx).await,
        ArtifactCommand::Cp(args) => cp::cp_command(&args, base_ctx).await,
    }
}
