use std::io::{self, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use fabro_sandbox::reconnect::reconnect;
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};
use fabro_workflow::sandbox_git::GIT_REMOTE;
use tracing::{debug, info};

use crate::args::{DiffArgs, GlobalArgs};
use crate::shared::print_json_pretty;
use crate::store;
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn run(args: DiffArgs, globals: &GlobalArgs) -> Result<()> {
    info!(run_id = %args.run, "Showing diff");
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run).await?;
    let run_id = run.run_id();
    let run_store = store::open_run_reader(&cli_settings.storage_dir(), &run_id).await?;

    let patch = resolve_diff(&run.path, &run_store, &args).await?;

    if globals.json {
        let mut value = serde_json::json!({
            "run_id": run_id,
            "node": args.node,
        });
        if args.shortstat {
            value["shortstat"] = patch.trim_end().into();
        } else if args.stat {
            value["stat"] = patch.trim_end().into();
        } else {
            value["diff"] = patch.into();
        }
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

async fn resolve_diff(
    _run_dir: &Path,
    run_store: &fabro_store::SlateRunStore,
    args: &DiffArgs,
) -> Result<String> {
    let state = run_store.state().await?;
    if let Some(ref node_id) = args.node {
        if let Some(visit) = state.list_node_visits(node_id).into_iter().max() {
            if let Some(node) = state.node(&fabro_store::NodeVisitRef { node_id, visit }) {
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
        .context("Failed to load start record from store")?;

    let base_sha = start
        .base_sha
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("This run was not git-checkpointed; no diff available"))?;

    if let Some(patch) = state.final_patch {
        debug!("Reading final.patch from store");
        return Ok(patch);
    }

    let run_concluded = state.conclusion.is_some();
    if run_concluded {
        bail!(
            "Run completed but no final.patch exists — the run may not have produced any changes"
        );
    }

    debug!("No final.patch found; attempting live diff from sandbox");
    let record = state
        .sandbox
        .context("Failed to load sandbox record from store")?;

    info!(provider = %record.provider, "Reconnecting to sandbox for live diff");
    let sandbox = reconnect(&record).await?;

    let cmd = build_live_diff_cmd(base_sha, args.stat, args.shortstat);
    debug!(cmd, "Running git diff in sandbox");

    let result = sandbox
        .exec_command(&cmd, 30_000, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run git diff in sandbox: {e}"))?;

    if result.exit_code != 0 {
        let stderr = result.stderr.trim();
        bail!("git diff failed (exit {}):\n{stderr}", result.exit_code);
    }

    Ok(result.stdout)
}

fn build_live_diff_cmd(base_sha: &str, stat: bool, shortstat: bool) -> String {
    let mut flags = String::new();
    if stat {
        flags.push_str(" --stat");
    }
    if shortstat {
        flags.push_str(" --shortstat");
    }
    let quoted_sha = shlex::try_quote(base_sha).map_or_else(
        |_| format!("'{}'", base_sha.replace('\'', "'\\''")),
        |q| q.to_string(),
    );
    format!("{GIT_REMOTE} add -N . && {GIT_REMOTE} diff{flags} {quoted_sha}")
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
