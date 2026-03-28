use std::io::{self, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use fabro_config::FabroSettingsExt;
use fabro_sandbox::SandboxRecordExt;
use fabro_sandbox::reconnect::reconnect;
use fabro_workflows::records::{StartRecord, StartRecordExt};
use fabro_workflows::run_lookup::{resolve_run, runs_base};
use fabro_workflows::sandbox_git::GIT_REMOTE;
use tracing::{debug, info};

use crate::args::DiffArgs;
use crate::cli_config::load_cli_settings;

pub(crate) async fn run(args: DiffArgs) -> Result<()> {
    info!(run_id = %args.run, "Showing diff");
    let cli_config = load_cli_settings(None)?;
    let base = runs_base(&cli_config.storage_dir());
    let run_dir = resolve_run(&base, &args.run)?.path;

    let patch = resolve_diff(&run_dir, &args).await?;

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

async fn resolve_diff(run_dir: &Path, args: &DiffArgs) -> Result<String> {
    if let Some(ref node_id) = args.node {
        debug!(node_id, "Reading per-node diff");
        let node_patch = run_dir.join("nodes").join(node_id).join("diff.patch");
        return std::fs::read_to_string(&node_patch).with_context(|| {
            format!("No diff found for node '{node_id}' — check the node ID and try again")
        });
    }

    let start = StartRecord::load(run_dir).context("Failed to load start.json")?;

    let base_sha = start
        .base_sha
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("This run was not git-checkpointed; no diff available"))?;

    let final_patch_path = run_dir.join("final.patch");
    if final_patch_path.exists() {
        debug!("Reading final.patch");
        return std::fs::read_to_string(&final_patch_path).context("Failed to read final.patch");
    }

    let conclusion_path = run_dir.join("conclusion.json");
    if conclusion_path.exists() {
        bail!(
            "Run completed but no final.patch exists — the run may not have produced any changes"
        );
    }

    debug!("No final.patch found; attempting live diff from sandbox");
    let sandbox_json = run_dir.join("sandbox.json");
    let record = fabro_sandbox::SandboxRecord::load(&sandbox_json).context(
        "Failed to load sandbox.json — was this run started with a recent version of arc?",
    )?;

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
    format!(
        "{} add -N . && {} diff{flags} {quoted_sha}",
        GIT_REMOTE, GIT_REMOTE
    )
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
