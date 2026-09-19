#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `run diff` command: blocking std::io::Write is the intended output mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `run diff` command: writes diff output to std::io::stdout directly"
)]

use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result, bail};
use fabro_types::{RunId, parse_blob_ref};
use tracing::{debug, info};

use crate::args::DiffArgs;
use crate::command_context::CommandContext;
use crate::server_client::{Client, RunProjection};
use crate::shared::print_json_pretty;

pub(crate) async fn run(args: DiffArgs, base_ctx: &CommandContext) -> Result<()> {
    info!(run_id = %args.run, "Showing diff");
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.id;
    let state = client.get_run_state(&run_id).await?;

    let patch = resolve_diff(&state, &args)?;
    let patch = resolve_patch_text(client.as_ref(), &run_id, patch).await?;

    if ctx.json_output() {
        let value = serde_json::json!({
            "run_id": run_id,
            "node": args.node,
            "diff": patch,
        });
        print_json_pretty(&value)?;
        return Ok(());
    }

    let is_tty = io::stdout().is_terminal();
    let mut stdout = io::stdout().lock();
    if is_tty {
        for line in patch.lines() {
            writeln!(stdout, "{}", colorize_diff_line(line))?;
        }
    } else {
        stdout.write_all(patch.as_bytes())?;
    }
    Ok(())
}

fn resolve_diff(state: &RunProjection, args: &DiffArgs) -> Result<String> {
    if let Some(ref node_id) = args.node {
        if let Some(visit) = state.list_node_visits(node_id).into_iter().max() {
            if let Some(node) = state.stage(&fabro_store::StageId::new(node_id, visit)) {
                if let Some(patch) = node.diff.clone() {
                    debug!(node_id, visit, "Reading per-node diff from projected state");
                    return Ok(patch);
                }
            }
        }

        bail!("No diff found for node '{node_id}' — check the node ID and try again");
    }

    let start = state
        .start
        .clone()
        .context("Failed to load start record from store")?;

    let base_sha = start
        .base_sha
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("This run was not git-checkpointed; no diff available"))?;

    if let Some(patch) = state
        .conclusion
        .as_ref()
        .and_then(|conclusion| conclusion.diff.patch.clone())
    {
        debug!("Reading stored diff from run state");
        return Ok(patch);
    }

    if state.conclusion.is_some() {
        bail!(
            "Run completed but no stored diff exists — the run may not have produced any changes"
        );
    }

    bail!(
        "Run is missing stored diff output since base commit {base_sha}; live sandbox diff is no longer supported"
    )
}

/// The patch as text: the projection holds a large patch as a
/// `blob://sha256/<hex>` reference into the run's blob table.
async fn resolve_patch_text(client: &Client, run_id: &RunId, patch: String) -> Result<String> {
    let Some(blob_hash) = parse_blob_ref(patch.trim()) else {
        return Ok(patch);
    };
    let bytes = client
        .read_run_blob(run_id, &blob_hash)
        .await?
        .with_context(|| format!("the diff's patch blob {blob_hash} is missing from the store"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn colorize_diff_line(line: &str) -> String {
    if line.starts_with("+++") || line.starts_with("---") {
        format!("\x1b[1m{line}\x1b[0m")
    } else if line.starts_with('+') {
        format!("\x1b[32m{line}\x1b[0m")
    } else if line.starts_with('-') {
        format!("\x1b[31m{line}\x1b[0m")
    } else if line.starts_with("@@") {
        format!("\x1b[36m{line}\x1b[0m")
    } else if line.starts_with("diff ") {
        format!("\x1b[1m{line}\x1b[0m")
    } else {
        line.to_string()
    }
}
