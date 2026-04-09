use std::path::Path;
use std::process::Command;

use fabro_checkpoint::git::Store;
use fabro_types::settings::v2::SettingsFile;

use crate::error::{FabroError, Result};
use tokio::task::{JoinError, spawn_blocking};
use tokio::time::timeout;

pub use fabro_checkpoint::META_BRANCH_PREFIX;
pub use fabro_checkpoint::author::GitAuthor;
pub use fabro_checkpoint::metadata::MetadataStore;

/// Branch prefix for workflow run branches (e.g. `fabro/run/{run_id}`).
pub const RUN_BRANCH_PREFIX: &str = "fabro/run/";

pub fn git_author_from_settings(settings: &SettingsFile) -> GitAuthor {
    settings
        .run_git_author()
        .map(GitAuthor::from)
        .unwrap_or_default()
}

fn git_error(msg: impl Into<String>) -> FabroError {
    FabroError::engine(msg.into())
}

/// Return a pre-configured `git` command with auto-maintenance disabled.
fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "maintenance.auto=0", "-c", "gc.auto=0"])
        .current_dir(dir);
    cmd
}

/// Assert the working directory is a clean git repo (no uncommitted changes).
pub fn ensure_clean(repo: &Path) -> Result<()> {
    tracing::debug!(path = %repo.display(), "Checking git cleanliness");
    let output = git_cmd(repo)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| git_error(format!("git status failed: {e}")))?;

    if !output.status.success() {
        return Err(git_error("not a git repository"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        return Err(git_error("working directory has uncommitted changes"));
    }

    Ok(())
}

/// Return the SHA of HEAD.
pub fn head_sha(repo: &Path) -> Result<String> {
    let output = git_cmd(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| git_error(format!("git rev-parse failed: {e}")))?;

    if !output.status.success() {
        return Err(git_error("git rev-parse HEAD failed"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Create a new branch at HEAD without checking it out.
pub fn create_branch(repo: &Path, name: &str) -> Result<()> {
    let output = git_cmd(repo)
        .args(["branch", "--force", name, "HEAD"])
        .output()
        .map_err(|e| git_error(format!("git branch failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git branch failed: {stderr}")));
    }

    Ok(())
}

/// Add a git worktree for the given branch at `path`.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    let output = git_cmd(repo)
        .args(["worktree", "add"])
        .arg(path)
        .arg(branch)
        .output()
        .map_err(|e| git_error(format!("git worktree add failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git worktree add failed: {stderr}")));
    }

    Ok(())
}

/// Remove a git worktree.
pub fn remove_worktree(repo: &Path, path: &Path) -> Result<()> {
    let output = git_cmd(repo)
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .output()
        .map_err(|e| git_error(format!("git worktree remove failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git worktree remove failed: {stderr}")));
    }

    Ok(())
}

/// Remove any stale worktree at `path` (best-effort), then add a fresh one.
pub fn replace_worktree(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    let _ = remove_worktree(repo, path);
    add_worktree(repo, path, branch)
}

/// Run a `git push` command and check for success.
fn run_git_push(cmd: &mut Command) -> Result<()> {
    let output = cmd
        .output()
        .map_err(|e| git_error(format!("git push failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git push failed: {stderr}")));
    }
    Ok(())
}

/// Push a local ref to an explicit remote URL.
///
/// Uses a URL (not a named remote) so the host repo's remote config is untouched.
/// Disables credential helpers so only the inline URL credentials are used.
pub fn push_ref(repo: &Path, url: &str, refname: &str) -> Result<()> {
    let redacted_url = if let Some(at_pos) = url.find('@') {
        format!("https://***@{}", &url[at_pos + 1..])
    } else {
        url.to_string()
    };
    tracing::info!(
        repo_dir = %repo.display(),
        url = %redacted_url,
        refname,
        "Pushing ref to remote"
    );
    run_git_push(git_cmd(repo).args(["-c", "credential.helper=", "push", url, refname]))
}

/// Push a local branch to the named remote using the user's configured credentials.
pub fn push_branch(repo: &Path, remote: &str, branch: &str) -> Result<()> {
    tracing::info!(
        repo_dir = %repo.display(),
        remote,
        branch,
        "Pushing branch to remote"
    );
    run_git_push(git_cmd(repo).args(["push", remote, branch]))
}

/// Push run and metadata branches to origin if a remote tracking branch exists.
///
/// Callers supply pre-built refspecs so they control force-push (`+` prefix).
#[allow(clippy::print_stderr)]
pub fn push_run_branches(
    store: &Store,
    probe_branch: &str,
    run_refspec: Option<&str>,
    meta_refspec: &str,
    label: &str,
) -> anyhow::Result<()> {
    let repo_path = store.repo_dir();
    let remote_ref = format!("refs/remotes/origin/{probe_branch}");
    if store.repo().find_reference(&remote_ref).is_err() {
        return Ok(());
    }
    eprintln!("Pushing {label} branches to origin...");
    if let Some(refspec) = run_refspec {
        push_branch(repo_path, "origin", refspec)
            .map_err(|e| anyhow::anyhow!("failed to push run branch: {e}"))?;
    }
    push_branch(repo_path, "origin", meta_refspec)
        .map_err(|e| anyhow::anyhow!("failed to push metadata branch: {e}"))?;
    eprintln!("Remote refs updated.");
    Ok(())
}

/// Error from [`blocking_push_with_timeout`].
pub enum BlockingPushError {
    /// The git push itself failed.
    Push(FabroError),
    /// The spawned blocking task panicked.
    Panicked(JoinError),
    /// The push did not complete within the timeout.
    TimedOut,
}

impl std::fmt::Display for BlockingPushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Push(e) => write!(f, "{e}"),
            Self::Panicked(e) => write!(f, "task panicked: {e}"),
            Self::TimedOut => write!(f, "timed out"),
        }
    }
}

/// Run a blocking git-push function with a timeout, flattening the triple-nested Result.
pub async fn blocking_push_with_timeout<F>(
    timeout_secs: u64,
    f: F,
) -> std::result::Result<(), BlockingPushError>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    match timeout(
        std::time::Duration::from_secs(timeout_secs),
        spawn_blocking(f),
    )
    .await
    {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(e))) => Err(BlockingPushError::Push(e)),
        Ok(Err(e)) => Err(BlockingPushError::Panicked(e)),
        Err(_) => Err(BlockingPushError::TimedOut),
    }
}

/// Returns true if the local branch has commits not yet on the remote.
/// On any git error (no remote ref, detached HEAD, etc.), returns true
/// so the caller falls back to pushing.
pub fn branch_needs_push(repo: &Path, remote: &str, branch: &str) -> bool {
    let local = git_cmd(repo)
        .args(["rev-parse", &format!("refs/heads/{branch}")])
        .output();
    let remote_ref = git_cmd(repo)
        .args(["rev-parse", &format!("refs/remotes/{remote}/{branch}")])
        .output();
    match (local, remote_ref) {
        (Ok(l), Ok(r)) if l.status.success() && r.status.success() => l.stdout != r.stdout,
        _ => true,
    }
}

/// Tri-state summary of the local repository's readiness for a workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitSyncStatus {
    /// Working tree is clean and the branch is pushed to the remote.
    Synced,
    /// Working tree is clean but the branch has unpushed commits
    /// (or push status could not be verified, e.g. detached HEAD).
    Unsynced,
    /// Working tree has uncommitted changes.
    Dirty,
}

impl GitSyncStatus {
    /// Whether the working tree has no uncommitted changes.
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Synced | Self::Unsynced)
    }
}

impl std::fmt::Display for GitSyncStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Synced => write!(f, "synced"),
            Self::Unsynced => write!(f, "unsynced (unpushed commits)"),
            Self::Dirty => write!(f, "dirty (uncommitted changes)"),
        }
    }
}

/// Determine the sync status of the repository relative to a remote.
pub fn sync_status(repo: &Path, remote: &str, branch: Option<&str>) -> GitSyncStatus {
    if ensure_clean(repo).is_err() {
        return GitSyncStatus::Dirty;
    }
    match branch {
        Some(b) if !branch_needs_push(repo, remote, b) => GitSyncStatus::Synced,
        _ => GitSyncStatus::Unsynced,
    }
}

/// Sanitize a string for use as a git ref component.
/// Lowercases, replaces non-alphanumeric chars with dashes, collapses runs.
pub fn sanitize_ref_component(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            result.push('-');
            prev_dash = true;
        }
    }
    result.trim_matches('-').to_string()
}

/// Filenames allowed in per-node directories on the shadow branch.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_dump::RunDump;
    use fabro_store::Database;
    use fabro_types::fixtures;
    use object_store::memory::InMemory;
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    /// Create a temporary git repo with an initial commit.
    fn init_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    fn test_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    #[test]
    fn ensure_clean_on_clean_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        assert!(ensure_clean(dir.path()).is_ok());
    }

    #[test]
    fn ensure_clean_fails_with_dirty_file() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("dirty.txt"), "hello").unwrap();
        let err = ensure_clean(dir.path()).unwrap_err();
        assert!(err.to_string().contains("uncommitted changes"));
    }

    #[test]
    fn ensure_clean_fails_on_non_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = ensure_clean(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not a git repository"));
    }

    #[test]
    fn head_sha_returns_40_char_hex() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let sha = head_sha(dir.path()).unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn create_branch_and_list() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        create_branch(dir.path(), "test-branch").unwrap();

        let output = Command::new("git")
            .args(["branch", "--list", "test-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("test-branch"));
    }

    #[test]
    fn add_and_remove_worktree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        create_branch(dir.path(), "wt-branch").unwrap();

        let wt_path = dir.path().join("my-worktree");
        add_worktree(dir.path(), &wt_path, "wt-branch").unwrap();
        assert!(wt_path.join(".git").exists());

        remove_worktree(dir.path(), &wt_path).unwrap();
        assert!(!wt_path.exists());
    }

    #[tokio::test]
    async fn scan_node_files_from_state_reconstructs_allowlisted_entries() {
        use crate::event::{Event, append_event};

        let store = test_store();
        let run = store.create_run(&fixtures::RUN_1).await.unwrap();
        append_event(
            &run,
            &fixtures::RUN_1,
            &Event::Prompt {
                stage: "work".into(),
                visit: 2,
                text: "hello".into(),
                mode: Some("prompt".into()),
                provider: Some("openai".into()),
                model: Some("gpt-5.4".into()),
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &fixtures::RUN_1,
            &Event::PromptCompleted {
                node_id: "work".into(),
                response: "world".into(),
                model: "gpt-5.4".into(),
                provider: "openai".into(),
                billing: None,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &fixtures::RUN_1,
            &Event::StageCompleted {
                node_id: "work".into(),
                name: "Work".into(),
                index: 2,
                duration_ms: 100,
                status: "success".into(),
                preferred_label: None,
                suggested_next_ids: Vec::new(),
                billing: None,
                failure: None,
                notes: None,
                files_touched: Vec::new(),
                context_updates: None,
                jump_to_node: None,
                context_values: None,
                node_visits: Some(std::collections::BTreeMap::from([("work".into(), 2)])),
                loop_failure_signatures: None,
                restart_failure_signatures: None,
                response: Some("world".into()),
                attempt: 1,
                max_attempts: 1,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &fixtures::RUN_1,
            &Event::CommandStarted {
                node_id: "work".into(),
                script: "echo hi".into(),
                command: "echo hi".into(),
                language: "shell".into(),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &fixtures::RUN_1,
            &Event::CommandCompleted {
                node_id: "work".into(),
                stdout: "hi\n".into(),
                stderr: String::new(),
                exit_code: Some(0),
                duration_ms: 10,
                timed_out: false,
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &fixtures::RUN_1,
            &Event::ParallelCompleted {
                node_id: "work".into(),
                visit: 2,
                duration_ms: 100,
                success_count: 1,
                failure_count: 0,
                results: vec![serde_json::json!({"id": "a"})],
            },
        )
        .await
        .unwrap();
        append_event(
            &run,
            &fixtures::RUN_1,
            &Event::CheckpointCompleted {
                node_id: "work".into(),
                status: "success".into(),
                current_node: "work".into(),
                completed_nodes: Vec::new(),
                node_retries: std::collections::BTreeMap::new(),
                context_values: std::collections::BTreeMap::new(),
                node_outcomes: std::collections::BTreeMap::new(),
                next_node_id: None,
                git_commit_sha: None,
                loop_failure_signatures: std::collections::BTreeMap::new(),
                restart_failure_signatures: std::collections::BTreeMap::new(),
                node_visits: std::collections::BTreeMap::from([("work".into(), 2)]),
                diff: Some("diff --git a/story.txt b/story.txt".into()),
            },
        )
        .await
        .unwrap();

        let state = run.state().await.unwrap();
        let files = RunDump::metadata_checkpoint(&state).git_entries().unwrap();
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert!(paths.contains(&"nodes/work-visit_2/prompt.md"));
        assert!(paths.contains(&"nodes/work-visit_2/response.md"));
        assert!(paths.contains(&"nodes/work-visit_2/status.json"));
        assert!(paths.contains(&"nodes/work-visit_2/provider_used.json"));
        assert!(paths.contains(&"nodes/work-visit_2/script_invocation.json"));
        assert!(paths.contains(&"nodes/work-visit_2/script_timing.json"));
        assert!(paths.contains(&"nodes/work-visit_2/parallel_results.json"));
    }

    #[test]
    fn sanitize_ref_component_lowercases() {
        assert_eq!(sanitize_ref_component("Hello"), "hello");
    }

    #[test]
    fn sanitize_ref_component_replaces_special_chars() {
        assert_eq!(sanitize_ref_component("a/b:c d"), "a-b-c-d");
    }

    #[test]
    fn sanitize_ref_component_collapses_consecutive_dashes() {
        assert_eq!(sanitize_ref_component("a///b"), "a-b");
    }

    #[test]
    fn sanitize_ref_component_trims_leading_trailing_dashes() {
        assert_eq!(sanitize_ref_component("--abc--"), "abc");
    }

    #[test]
    fn sanitize_ref_component_mixed() {
        assert_eq!(sanitize_ref_component("My Node!@#123"), "my-node-123");
    }

    #[test]
    fn replace_worktree_on_clean_path() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        create_branch(dir.path(), "rw-branch").unwrap();

        let wt_path = dir.path().join("rw-worktree");
        replace_worktree(dir.path(), &wt_path, "rw-branch").unwrap();
        assert!(wt_path.join(".git").exists());

        remove_worktree(dir.path(), &wt_path).unwrap();
    }

    #[test]
    fn replace_worktree_replaces_stale() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        create_branch(dir.path(), "stale-branch").unwrap();

        let wt_path = dir.path().join("stale-wt");
        add_worktree(dir.path(), &wt_path, "stale-branch").unwrap();
        assert!(wt_path.join(".git").exists());

        // Calling replace_worktree again succeeds (removes stale, re-creates)
        replace_worktree(dir.path(), &wt_path, "stale-branch").unwrap();
        assert!(wt_path.join(".git").exists());

        remove_worktree(dir.path(), &wt_path).unwrap();
    }

    #[test]
    fn push_ref_to_bare_remote() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo");
        let remote_dir = dir.path().join("remote.git");

        // Create a bare remote
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote_dir)
            .output()
            .unwrap();

        // Create a local repo with origin pointing at the bare remote
        Command::new("git")
            .args(["init"])
            .arg(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote_dir)
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(&repo_dir)
            .output()
            .unwrap();

        // Create a branch and push it via push_ref
        create_branch(&repo_dir, "test-push").unwrap();
        let url = format!("file://{}", remote_dir.display());
        push_ref(&repo_dir, &url, "refs/heads/test-push").unwrap();

        // Verify the remote now has the branch
        let output = Command::new("git")
            .args(["branch", "--list", "test-push"])
            .current_dir(&remote_dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("test-push"),
            "remote should have test-push branch"
        );
    }

    #[test]
    fn push_branch_to_remote() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo");
        let remote_dir = dir.path().join("remote.git");

        // Create a bare remote
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote_dir)
            .output()
            .unwrap();

        // Create a local repo with origin pointing at the bare remote
        Command::new("git")
            .args(["init"])
            .arg(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote_dir)
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(&repo_dir)
            .output()
            .unwrap();

        // Rename default branch to "main" for predictability
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&repo_dir)
            .output()
            .unwrap();

        // Push using push_branch
        push_branch(&repo_dir, "origin", "main").unwrap();

        // Verify the remote now has the commit
        let output = Command::new("git")
            .args(["branch", "--list", "main"])
            .current_dir(&remote_dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("main"), "remote should have main branch");
    }

    #[test]
    fn push_branch_fails_for_nonexistent_remote() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let result = push_branch(dir.path(), "nonexistent", "main");
        assert!(result.is_err());
    }

    #[test]
    fn branch_needs_push_when_ahead() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo");
        let remote_dir = dir.path().join("remote.git");

        Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["init"])
            .arg(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote_dir)
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&repo_dir)
            .output()
            .unwrap();

        // Push once to establish remote tracking
        push_branch(&repo_dir, "origin", "main").unwrap();

        // Make another commit locally (now ahead of remote)
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "second",
            ])
            .current_dir(&repo_dir)
            .output()
            .unwrap();

        assert!(branch_needs_push(&repo_dir, "origin", "main"));
    }

    #[test]
    fn branch_needs_push_when_in_sync() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo");
        let remote_dir = dir.path().join("remote.git");

        Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["init"])
            .arg(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote_dir)
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&repo_dir)
            .output()
            .unwrap();

        push_branch(&repo_dir, "origin", "main").unwrap();

        assert!(!branch_needs_push(&repo_dir, "origin", "main"));
    }

    #[test]
    fn branch_needs_push_when_no_remote_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path();

        init_repo(repo_dir);

        // No remote at all — should return true (safe default)
        assert!(branch_needs_push(repo_dir, "origin", "main"));
    }

    #[test]
    fn metadata_branch_name_uses_meta_prefix() {
        assert_eq!(MetadataStore::branch_name("abc-123"), "fabro/meta/abc-123");
    }

    #[test]
    fn meta_branch_prefix_constant() {
        assert!(MetadataStore::branch_name("x").starts_with(META_BRANCH_PREFIX));
    }
}
