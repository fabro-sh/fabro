use std::io::{self, IsTerminal, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Args;
use tracing::{debug, info};

use crate::cli::runs::{default_runs_base, find_run_by_prefix};
use crate::engine::GIT_REMOTE;
use crate::manifest::Manifest;
use crate::sandbox_record::SandboxRecord;

#[derive(Args)]
pub struct DiffArgs {
    /// Run ID or prefix
    pub run: String,
    /// Show diff for a specific node
    #[arg(long)]
    pub node: Option<String>,
    /// Show diffstat instead of full patch (live diffs only)
    #[arg(long)]
    pub stat: bool,
    /// Show only files-changed/insertions/deletions summary (live diffs only)
    #[arg(long)]
    pub shortstat: bool,
}

pub async fn diff_command(args: DiffArgs) -> Result<()> {
    info!(run_id = %args.run, "Showing diff");
    let base = default_runs_base();
    let run_dir = find_run_by_prefix(&base, &args.run)?;

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
    // --node: read per-node diff.patch
    if let Some(ref node_id) = args.node {
        debug!(node_id, "Reading per-node diff");
        let node_patch = run_dir.join("nodes").join(node_id).join("diff.patch");
        return std::fs::read_to_string(&node_patch).with_context(|| {
            format!("No diff found for node '{node_id}' — check the node ID and try again")
        });
    }

    let manifest =
        Manifest::load(&run_dir.join("manifest.json")).context("Failed to load manifest.json")?;

    let base_sha = manifest
        .base_sha
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("This run was not git-checkpointed; no diff available"))?;

    // Completed run with final.patch
    let final_patch_path = run_dir.join("final.patch");
    if final_patch_path.exists() {
        debug!("Reading final.patch");
        return std::fs::read_to_string(&final_patch_path).context("Failed to read final.patch");
    }

    // Check if the run has concluded (no final.patch means no changes or error)
    let conclusion_path = run_dir.join("conclusion.json");
    if conclusion_path.exists() {
        bail!(
            "Run completed but no final.patch exists — the run may not have produced any changes"
        );
    }

    // In-progress run: reconnect to sandbox and run git diff
    debug!("No final.patch found; attempting live diff from sandbox");
    let sandbox_json = run_dir.join("sandbox.json");
    let record = SandboxRecord::load(&sandbox_json).context(
        "Failed to load sandbox.json — was this run started with a recent version of arc?",
    )?;

    info!(provider = %record.provider, "Reconnecting to sandbox for live diff");
    let sandbox = crate::cli::cp::reconnect(&record).await?;

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
    // `git add -N .` marks untracked files as intent-to-add so they appear in the diff.
    // Without this, files created by write_file (which doesn't git-add) are invisible.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_manifest(dir: &Path, base_sha: Option<&str>) {
        let manifest = serde_json::json!({
            "run_id": "test-run-001",
            "workflow_name": "test",
            "goal": "test goal",
            "start_time": "2025-01-01T00:00:00Z",
            "node_count": 1,
            "edge_count": 0,
            "base_sha": base_sha,
            "labels": {},
        });
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn create_conclusion(dir: &Path) {
        let conclusion = serde_json::json!({
            "timestamp": "2025-01-01T00:01:00Z",
            "status": "success",
            "duration_ms": 60000,
        });
        fs::write(
            dir.join("conclusion.json"),
            serde_json::to_string_pretty(&conclusion).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn completed_run_with_final_patch() {
        let dir = tempfile::tempdir().unwrap();
        create_manifest(dir.path(), Some("abc123"));

        let patch_content = "diff --git a/file.txt b/file.txt\n--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new\n";
        fs::write(dir.path().join("final.patch"), patch_content).unwrap();

        let args = DiffArgs {
            run: String::new(),
            node: None,
            stat: false,
            shortstat: false,
        };
        let result = resolve_diff(dir.path(), &args).await.unwrap();
        assert_eq!(result, patch_content);
    }

    #[tokio::test]
    async fn per_node_diff() {
        let dir = tempfile::tempdir().unwrap();
        let node_dir = dir.path().join("nodes").join("work");
        fs::create_dir_all(&node_dir).unwrap();

        let patch_content = "diff --git a/src/main.rs b/src/main.rs\n+added line\n";
        fs::write(node_dir.join("diff.patch"), patch_content).unwrap();

        let args = DiffArgs {
            run: String::new(),
            node: Some("work".to_string()),
            stat: false,
            shortstat: false,
        };
        let result = resolve_diff(dir.path(), &args).await.unwrap();
        assert_eq!(result, patch_content);
    }

    #[tokio::test]
    async fn no_base_sha_errors() {
        let dir = tempfile::tempdir().unwrap();
        create_manifest(dir.path(), None);

        let args = DiffArgs {
            run: String::new(),
            node: None,
            stat: false,
            shortstat: false,
        };
        let err = resolve_diff(dir.path(), &args).await.unwrap_err();
        assert!(
            err.to_string().contains("not git-checkpointed"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn completed_run_no_final_patch() {
        let dir = tempfile::tempdir().unwrap();
        create_manifest(dir.path(), Some("abc123"));
        create_conclusion(dir.path());

        let args = DiffArgs {
            run: String::new(),
            node: None,
            stat: false,
            shortstat: false,
        };
        let err = resolve_diff(dir.path(), &args).await.unwrap_err();
        assert!(err.to_string().contains("no final.patch"), "got: {err}");
    }

    #[tokio::test]
    async fn node_diff_not_found() {
        let dir = tempfile::tempdir().unwrap();

        let args = DiffArgs {
            run: String::new(),
            node: Some("nonexistent".to_string()),
            stat: false,
            shortstat: false,
        };
        let err = resolve_diff(dir.path(), &args).await.unwrap_err();
        assert!(err.to_string().contains("nonexistent"), "got: {err}");
    }

    #[test]
    fn colorize_added_line() {
        let result = colorize_diff_line("+added");
        assert!(result.contains("\x1b[32m"));
        assert!(result.contains("+added"));
    }

    #[test]
    fn colorize_removed_line() {
        let result = colorize_diff_line("-removed");
        assert!(result.contains("\x1b[31m"));
        assert!(result.contains("-removed"));
    }

    #[test]
    fn colorize_hunk_header() {
        let result = colorize_diff_line("@@ -1,3 +1,4 @@");
        assert!(result.contains("\x1b[36m"));
    }

    #[test]
    fn colorize_diff_header() {
        let result = colorize_diff_line("diff --git a/file b/file");
        assert!(result.contains("\x1b[1m"));
    }

    #[test]
    fn colorize_file_header() {
        let plus = colorize_diff_line("+++ b/file.txt");
        assert!(plus.contains("\x1b[1m"), "got: {plus}");

        let minus = colorize_diff_line("--- a/file.txt");
        assert!(minus.contains("\x1b[1m"), "got: {minus}");
    }

    #[test]
    fn colorize_context_line_unchanged() {
        let result = colorize_diff_line(" context line");
        assert_eq!(result, " context line");
    }

    #[test]
    fn build_live_diff_cmd_includes_working_tree() {
        let cmd = build_live_diff_cmd("abc123", false, false);
        assert_eq!(
            cmd,
            "git -c maintenance.auto=0 -c gc.auto=0 add -N . && git -c maintenance.auto=0 -c gc.auto=0 diff abc123"
        );
    }

    #[test]
    fn build_live_diff_cmd_stat() {
        let cmd = build_live_diff_cmd("abc123", true, false);
        assert_eq!(
            cmd,
            "git -c maintenance.auto=0 -c gc.auto=0 add -N . && git -c maintenance.auto=0 -c gc.auto=0 diff --stat abc123"
        );
    }

    #[test]
    fn build_live_diff_cmd_shortstat() {
        let cmd = build_live_diff_cmd("abc123", false, true);
        assert_eq!(
            cmd,
            "git -c maintenance.auto=0 -c gc.auto=0 add -N . && git -c maintenance.auto=0 -c gc.auto=0 diff --shortstat abc123"
        );
    }
}
