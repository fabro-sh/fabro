use std::path::Path;
use std::process::Command;

pub use fabro_checkpoint::author::GitAuthor;
use fabro_redact::DisplaySafeUrl;
use fabro_types::{DirtyStatus, GitContext, WorkflowSettings};
use tokio::task::{JoinError, spawn_blocking};
use tokio::time::timeout;

use crate::error::{Error, Result};

/// Branch prefix for workflow run branches (e.g. `fabro/run/{run_id}`).
pub const RUN_BRANCH_PREFIX: &str = "fabro/run/";

/// A local checkout could not be inspected without changing it.
#[derive(Debug, thiserror::Error)]
pub enum GitObservationError {
    #[error("failed to discover the local Git repository")]
    Discover {
        #[source]
        source: git2::Error,
    },
    #[error("failed to read the local Git repository HEAD")]
    Head {
        #[source]
        source: git2::Error,
    },
    #[error("failed to read the local Git repository origin")]
    Origin {
        #[source]
        source: git2::Error,
    },
    #[error("failed to read the local Git repository status")]
    Status {
        #[source]
        source: git2::Error,
    },
}

/// Observe the current branch, commit, origin, and dirty state of a local
/// checkout without invoking Git commands, contacting a remote, or mutating
/// the repository. Non-repositories, unborn repositories, and detached HEADs
/// have no usable [`GitContext`] and return `Ok(None)`.
pub fn observe_git_context(
    path: &Path,
) -> std::result::Result<Option<GitContext>, GitObservationError> {
    let repo = match git2::Repository::discover(path) {
        Ok(repo) => repo,
        Err(source) if source.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(source) => return Err(GitObservationError::Discover { source }),
    };
    let head = match repo.head() {
        Ok(head) => head,
        Err(source)
            if matches!(
                source.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
            ) =>
        {
            return Ok(None);
        }
        Err(source) => return Err(GitObservationError::Head { source }),
    };
    if !head.is_branch() {
        return Ok(None);
    }
    let Some(branch) = head.shorthand().filter(|branch| !branch.is_empty()) else {
        return Ok(None);
    };
    let branch = branch.to_string();
    let sha = head.target().map(|oid| oid.to_string());
    drop(head);

    let origin_url = match repo.find_remote("origin") {
        Ok(remote) => remote.url().map(sanitized_origin_url).unwrap_or_default(),
        Err(source) if source.code() == git2::ErrorCode::NotFound => String::new(),
        Err(source) => return Err(GitObservationError::Origin { source }),
    };
    let mut status_options = git2::StatusOptions::new();
    status_options
        .include_untracked(true)
        .no_refresh(true)
        .update_index(false);
    let statuses = repo
        .statuses(Some(&mut status_options))
        .map_err(|source| GitObservationError::Status { source })?;
    let dirty = if statuses
        .iter()
        .any(|entry| entry.status() != git2::Status::CURRENT)
    {
        DirtyStatus::Dirty
    } else {
        DirtyStatus::Clean
    };

    Ok(Some(GitContext {
        origin_url,
        branch,
        sha,
        dirty,
    }))
}

fn sanitized_origin_url(value: &str) -> String {
    let normalized = fabro_github::normalize_repo_origin_url(value);
    let Ok(url) = DisplaySafeUrl::parse(&normalized) else {
        return String::new();
    };
    let mut url = url.without_credentials().into_owned();
    url.set_query(None);
    url.set_fragment(None);
    fabro_github::normalize_repo_origin_url(url.as_str())
}

pub fn git_author_from_settings(settings: &WorkflowSettings) -> GitAuthor {
    settings
        .run
        .git
        .author
        .clone()
        .map(|author| GitAuthor::from(&author))
        .unwrap_or_default()
}

fn git_error(msg: impl Into<String>) -> Error {
    Error::engine(msg.into())
}

/// Return a pre-configured `git` command with auto-maintenance disabled.
#[expect(
    clippy::disallowed_methods,
    reason = "This shared synchronous git helper layer is used by sync code; async callers must wrap it in spawn_blocking."
)]
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
        .map_err(|e| Error::engine_with_source("git status failed", e))?;

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
        .map_err(|e| Error::engine_with_source("git rev-parse failed", e))?;

    if !output.status.success() {
        return Err(git_error("git rev-parse HEAD failed"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run a `git push` command and check for success.
fn run_git_push(cmd: &mut Command) -> Result<()> {
    let output = cmd
        .output()
        .map_err(|e| Error::engine_with_source("git push failed", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git push failed: {stderr}")));
    }
    Ok(())
}

/// Push a local ref to an explicit remote URL.
///
/// Uses a URL (not a named remote) so the host repo's remote config is
/// untouched. Disables credential helpers so only the inline URL credentials
/// are used.
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

/// Push a local branch to the named remote using the user's configured
/// credentials.
pub fn push_branch(repo: &Path, remote: &str, branch: &str) -> Result<()> {
    tracing::info!(
        repo_dir = %repo.display(),
        remote,
        branch,
        "Pushing branch to remote"
    );
    run_git_push(git_cmd(repo).args(["push", remote, branch]))
}

/// Push a local branch to the named remote without allowing Git to prompt.
pub fn push_branch_noninteractive(repo: &Path, remote: &str, branch: &str) -> Result<()> {
    tracing::info!(
        repo_dir = %repo.display(),
        remote,
        branch,
        "Pushing branch to remote without terminal prompts"
    );
    run_git_push(
        git_cmd(repo)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["push", remote, branch]),
    )
}

/// Read the exact commit currently advertised for a remote branch without
/// allowing Git to prompt for credentials.
///
/// This queries the remote itself rather than trusting the checkout's local
/// remote-tracking ref, which may be stale or may have been rewritten locally.
pub fn remote_branch_sha_noninteractive(
    repo: &Path,
    remote: &str,
    branch: &str,
) -> Result<Option<String>> {
    let branch_ref = format!("refs/heads/{branch}");
    let output = git_cmd(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["ls-remote", "--refs", remote, &branch_ref])
        .output()
        .map_err(|e| Error::engine_with_source("git ls-remote failed", e))?;
    if !output.status.success() {
        return Err(git_error("git ls-remote failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut fields = line.split_whitespace();
        let (Some(sha), Some(observed_ref), None) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if observed_ref == branch_ref {
            return Ok(Some(sha.to_owned()));
        }
    }
    Ok(None)
}

/// Error from [`blocking_push_with_timeout`].
pub enum BlockingPushError {
    /// The git push itself failed.
    Push(Error),
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

/// Run a blocking git-push function with a timeout, flattening the
/// triple-nested Result.
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

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests write git state fixtures to disk"
)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_dump::RunDump;
    use fabro_store::Database;
    use fabro_types::{CommandTermination, StageModelUsage, fixtures, test_support};
    use object_store::memory::InMemory;

    use super::*;

    /// Create a temporary git repo with an initial commit.
    #[expect(
        clippy::disallowed_methods,
        reason = "This synchronous test helper shells out to git while constructing fixture repositories."
    )]
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

    #[test]
    fn observe_git_context_is_read_only_and_reports_local_state() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let repo = git2::Repository::open(dir.path()).unwrap();
        repo.remote("origin", "git@github.com:fabro-sh/fabro.git")
            .unwrap();
        drop(repo);
        let git_dir = dir.path().join(".git");
        let head_before = fs::read(git_dir.join("HEAD")).unwrap();
        let index_before = fs::read(git_dir.join("index")).unwrap();
        let config_before = fs::read(git_dir.join("config")).unwrap();

        let observed = observe_git_context(dir.path()).unwrap().unwrap();
        assert!(!observed.branch.is_empty());
        assert_eq!(observed.origin_url, "https://github.com/fabro-sh/fabro");
        assert_eq!(observed.sha.as_deref().map(str::len), Some(40));
        assert_eq!(observed.dirty, DirtyStatus::Clean);
        assert_eq!(fs::read(git_dir.join("HEAD")).unwrap(), head_before);
        assert_eq!(fs::read(git_dir.join("index")).unwrap(), index_before);
        assert_eq!(fs::read(git_dir.join("config")).unwrap(), config_before);
        assert!(!git_dir.join("HEAD.lock").exists());
        assert!(!git_dir.join("index.lock").exists());
        assert!(!git_dir.join("config.lock").exists());

        fs::write(dir.path().join("untracked.txt"), "changed").unwrap();
        let observed = observe_git_context(dir.path()).unwrap().unwrap();
        assert_eq!(observed.dirty, DirtyStatus::Dirty);
        assert_eq!(fs::read(git_dir.join("HEAD")).unwrap(), head_before);
        assert_eq!(fs::read(git_dir.join("index")).unwrap(), index_before);
        assert_eq!(fs::read(git_dir.join("config")).unwrap(), config_before);
    }

    #[test]
    fn observe_git_context_never_persists_remote_credentials() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let repo = git2::Repository::open(dir.path()).unwrap();
        repo.remote(
            "origin",
            "http://run-user:secret@example.com/acme/widgets.git?token=secret#fragment",
        )
        .unwrap();
        drop(repo);

        let observed = observe_git_context(dir.path()).unwrap().unwrap();

        assert_eq!(observed.origin_url, "http://example.com/acme/widgets");
        assert!(!observed.origin_url.contains("secret"));
        assert!(!observed.origin_url.contains("token"));
    }

    #[test]
    fn observe_git_context_accepts_a_non_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(observe_git_context(dir.path()).unwrap(), None);
    }

    #[test]
    fn observe_git_context_handles_unborn_detached_and_nested_checkouts() {
        let unborn = tempfile::tempdir().unwrap();
        git2::Repository::init(unborn.path()).unwrap();
        assert_eq!(observe_git_context(unborn.path()).unwrap(), None);

        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let observed = observe_git_context(&nested).unwrap().unwrap();
        assert!(observed.origin_url.is_empty());
        assert!(!observed.branch.is_empty());

        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().target().unwrap();
        repo.set_head_detached(head).unwrap();
        assert_eq!(observe_git_context(dir.path()).unwrap(), None);
    }

    fn test_store() -> Arc<Database> {
        Arc::new(fabro_store::test_support::test_database(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
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

    #[tokio::test]
    async fn scan_node_files_from_state_reconstructs_allowlisted_entries() {
        use crate::event::{Event, append_event};

        let store = test_store();
        let run = store.create_run(&fixtures::RUN_1).await.unwrap();
        append_event(&run, &fixtures::RUN_1, &Event::RunCreated {
            run_id:              fixtures::RUN_1,
            title:               None,
            settings:            serde_json::to_value(fabro_types::WorkflowSettings::default())
                .unwrap(),
            graph:               serde_json::to_value(fabro_types::Graph::new("test")).unwrap(),
            workflow_source:     None,
            labels:              std::collections::BTreeMap::default(),
            source_directory:    None,
            workflow_slug:       None,
            workflow_version_id: None,
            target:              None,
            automation:          None,
            provenance:          test_support::test_run_provenance(),
            manifest_blob:       None,
            spec_blob:           None,
            git:                 None,
            fork_source_ref:     None,
            retried_from:        None,
            parent_id:           None,
            web_url:             None,
        })
        .await
        .unwrap();
        append_event(&run, &fixtures::RUN_1, &Event::Prompt {
            stage:            "work".into(),
            visit:            2,
            text:             "hello".into(),
            mode:             Some(StageModelUsage::MODE_PROMPT.to_string()),
            provider:         Some("openai".into()),
            model:            Some("gpt-5.4".into()),
            reasoning_effort: None,
            speed:            None,
        })
        .await
        .unwrap();
        append_event(&run, &fixtures::RUN_1, &Event::PromptCompleted {
            node_id:  "work".into(),
            response: "world".into(),
            model:    "gpt-5.4".into(),
            provider: "openai".into(),
            billing:  None,
        })
        .await
        .unwrap();
        append_event(&run, &fixtures::RUN_1, &Event::StageCompleted {
            node_id: "work".into(),
            name: "Work".into(),
            index: 2,
            timing: fabro_types::StageTiming::wall_only(100),
            status: "succeeded".into(),
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
        })
        .await
        .unwrap();
        append_event(&run, &fixtures::RUN_1, &Event::CommandStarted {
            node_id:    "work".into(),
            script:     "echo hi".into(),
            command:    "echo hi".into(),
            language:   "shell".into(),
            timeout_ms: None,
        })
        .await
        .unwrap();
        append_event(&run, &fixtures::RUN_1, &Event::CommandCompleted {
            node_id:        "work".into(),
            output:         "hi\n".into(),
            exit_code:      Some(0),
            duration_ms:    10,
            termination:    CommandTermination::Exited,
            output_bytes:   3,
            live_streaming: true,
        })
        .await
        .unwrap();
        append_event(&run, &fixtures::RUN_1, &Event::ParallelCompleted {
            node_id:       "work".into(),
            visit:         2,
            duration_ms:   100,
            success_count: 1,
            failure_count: 0,
            results:       vec![fabro_types::ParallelBranchResult {
                id:              "a".to_string(),
                index:           Some(0),
                item_label:      None,
                status:          fabro_types::StageOutcome::Succeeded,
                context_updates: std::collections::BTreeMap::new(),
            }],
        })
        .await
        .unwrap();
        append_event(&run, &fixtures::RUN_1, &Event::CheckpointCompleted {
            graph_visit: None,
            resumed_from_stage_id: None,
            node_id: "work".into(),
            status: "succeeded".into(),
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
            diff_summary: None,
        })
        .await
        .unwrap();

        let state = run.state().await.unwrap();
        let files = RunDump::from_projection(&state)
            .unwrap()
            .git_entries()
            .unwrap();
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert!(paths.contains(&"stages/001-work@2/prompt.md"));
        assert!(paths.contains(&"stages/001-work@2/response.md"));
        assert!(paths.contains(&"stages/001-work@2/status.json"));
        assert!(paths.contains(&"stages/001-work@2/provider_used.json"));
        assert!(paths.contains(&"stages/001-work@2/script_invocation.json"));
        assert!(paths.contains(&"stages/001-work@2/script_timing.json"));
        assert!(paths.contains(&"stages/001-work@2/parallel_results.json"));
    }

    #[test]
    fn push_branch_fails_for_nonexistent_remote() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let result = push_branch(dir.path(), "nonexistent", "main");
        assert!(result.is_err());
    }

    #[test]
    fn branch_needs_push_when_no_remote_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path();

        init_repo(repo_dir);

        // No remote at all — should return true (safe default)
        assert!(branch_needs_push(repo_dir, "origin", "main"));
    }
}
