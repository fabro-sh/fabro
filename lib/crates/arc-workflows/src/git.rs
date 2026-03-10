use std::path::Path;
use std::process::Command;

use arc_git_storage::branchstore::BranchStore;
use arc_git_storage::gitobj::Store;
use git2::{Repository, Signature};

use crate::checkpoint::Checkpoint;
use crate::error::{ArcError, Result};

/// Resolved git author identity for checkpoint commits.
#[derive(Debug, Clone, PartialEq)]
pub struct GitAuthor {
    pub name: String,
    pub email: String,
}

impl Default for GitAuthor {
    fn default() -> Self {
        Self {
            name: "arc".into(),
            email: "arc@local".into(),
        }
    }
}

impl GitAuthor {
    /// Create a `GitAuthor` from optional name/email, falling back to defaults.
    pub fn from_options(name: Option<String>, email: Option<String>) -> Self {
        let defaults = Self::default();
        Self {
            name: name.unwrap_or(defaults.name),
            email: email.unwrap_or(defaults.email),
        }
    }
}

fn git_error(msg: impl Into<String>) -> ArcError {
    ArcError::engine(msg.into())
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

/// Error from [`blocking_push_with_timeout`].
pub enum BlockingPushError {
    /// The git push itself failed.
    Push(crate::error::ArcError),
    /// The spawned blocking task panicked.
    Panicked(tokio::task::JoinError),
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
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::task::spawn_blocking(f),
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

/// Git-native metadata storage for pipeline runs.
///
/// Stores checkpoint data, manifests, and graph DOT on an orphan branch
/// (`arc/{run_id}`) so that runs can be resumed from git alone.
pub struct MetadataStore {
    repo_path: std::path::PathBuf,
    author: GitAuthor,
}

impl MetadataStore {
    pub fn new(repo_path: impl Into<std::path::PathBuf>, author: &GitAuthor) -> Self {
        Self {
            repo_path: repo_path.into(),
            author: author.clone(),
        }
    }

    /// Returns the branch ref name for a run: `refs/arc/{run_id}`.
    pub fn branch_name(run_id: &str) -> String {
        format!("refs/arc/{run_id}")
    }

    fn open_store(&self) -> Result<(Store, Signature<'static>)> {
        let repo = Repository::discover(&self.repo_path)
            .map_err(|e| git_error(format!("failed to open repo: {e}")))?;
        let store = Store::new(repo);
        let sig = Signature::now(&self.author.name, &self.author.email)
            .map_err(|e| git_error(format!("failed to create signature: {e}")))?;
        Ok((store, sig))
    }

    /// Initialize a run's metadata branch with manifest and graph DOT.
    pub fn init_run(&self, run_id: &str, manifest_json: &[u8], graph_dot: &[u8]) -> Result<()> {
        let (store, sig) = self.open_store()?;
        let branch = Self::branch_name(run_id);
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.ensure_branch()
            .map_err(|e| git_error(format!("ensure_branch failed: {e}")))?;
        bs.write_entries(
            &[("manifest.json", manifest_json), ("graph.dot", graph_dot)],
            "init run",
        )
        .map_err(|e| git_error(format!("write_entries failed: {e}")))?;
        Ok(())
    }

    /// Write checkpoint data (and optional artifacts) to the metadata branch.
    /// Returns the SHA of the new commit on the shadow branch.
    pub fn write_checkpoint(
        &self,
        run_id: &str,
        checkpoint_json: &[u8],
        artifacts: &[(&str, &[u8])],
    ) -> Result<String> {
        let (store, sig) = self.open_store()?;
        let branch = Self::branch_name(run_id);
        let bs = BranchStore::new(&store, &branch, &sig);
        let mut entries: Vec<(&str, &[u8])> = vec![("checkpoint.json", checkpoint_json)];
        entries.extend_from_slice(artifacts);
        let oid = bs
            .write_entries(&entries, "checkpoint")
            .map_err(|e| git_error(format!("write_entries failed: {e}")))?;
        Ok(oid.to_string())
    }

    /// Read a single file from the metadata branch. Returns `None` if branch or path doesn't exist.
    fn read_file(repo_path: &Path, run_id: &str, path: &str) -> Result<Option<Vec<u8>>> {
        let repo = match Repository::discover(repo_path) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let store = Store::new(repo);
        let sig = Signature::now("arc", "arc@local")
            .map_err(|e| git_error(format!("failed to create signature: {e}")))?;
        let branch = Self::branch_name(run_id);
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.read_entry(path)
            .map_err(|e| git_error(format!("read_entry failed: {e}")))
    }

    /// Read a checkpoint from the metadata branch. Returns `None` if branch or file doesn't exist.
    pub fn read_checkpoint(repo_path: &Path, run_id: &str) -> Result<Option<Checkpoint>> {
        match Self::read_file(repo_path, run_id, "checkpoint.json")? {
            Some(bytes) => {
                let cp: Checkpoint = serde_json::from_slice(&bytes)
                    .map_err(|e| ArcError::Checkpoint(format!("deserialize failed: {e}")))?;
                Ok(Some(cp))
            }
            None => Ok(None),
        }
    }

    /// Read the manifest from the metadata branch. Returns `None` if not found.
    pub fn read_manifest(
        repo_path: &Path,
        run_id: &str,
    ) -> Result<Option<crate::manifest::Manifest>> {
        match Self::read_file(repo_path, run_id, "manifest.json")? {
            Some(bytes) => {
                let manifest: crate::manifest::Manifest = serde_json::from_slice(&bytes)
                    .map_err(|e| git_error(format!("manifest deserialize failed: {e}")))?;
                Ok(Some(manifest))
            }
            None => Ok(None),
        }
    }

    /// Read the graph DOT source from the metadata branch. Returns `None` if not found.
    pub fn read_graph_dot(repo_path: &Path, run_id: &str) -> Result<Option<String>> {
        match Self::read_file(repo_path, run_id, "graph.dot")? {
            Some(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).to_string())),
            None => Ok(None),
        }
    }

    /// Read an artifact from the metadata branch. Returns `None` if not found.
    pub fn read_artifact(repo_path: &Path, run_id: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Self::read_file(repo_path, run_id, &format!("artifacts/{key}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

    // --- MetadataStore tests ---

    #[test]
    fn metadata_store_init_run_and_read() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        let manifest = br#"{"run_id":"RUN1","workflow_name":"test","goal":"g","start_time":"2025-01-01T00:00:00Z","node_count":2,"edge_count":1}"#;
        let dot = b"digraph { start -> end }";
        store.init_run("RUN1", manifest, dot).unwrap();

        let read_manifest = MetadataStore::read_manifest(dir.path(), "RUN1")
            .unwrap()
            .unwrap();
        assert_eq!(read_manifest.run_id, "RUN1");
        assert_eq!(read_manifest.workflow_name, "test");

        let read_dot = MetadataStore::read_graph_dot(dir.path(), "RUN1")
            .unwrap()
            .unwrap();
        assert_eq!(read_dot, "digraph { start -> end }");
    }

    #[test]
    fn metadata_store_write_and_read_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        store.init_run("RUN2", b"{}", b"digraph {}").unwrap();

        let ctx = crate::context::Context::new();
        ctx.set("goal", serde_json::json!("test"));
        let cp = crate::checkpoint::Checkpoint::from_context(
            &ctx,
            "node_a",
            vec!["start".to_string()],
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            Some("node_b".to_string()),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );
        let cp_json = serde_json::to_vec_pretty(&cp).unwrap();
        store.write_checkpoint("RUN2", &cp_json, &[]).unwrap();

        let loaded = MetadataStore::read_checkpoint(dir.path(), "RUN2")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.current_node, "node_a");
        assert_eq!(loaded.completed_nodes, vec!["start"]);
        assert_eq!(loaded.next_node_id.as_deref(), Some("node_b"));
        assert_eq!(
            loaded.context_values.get("goal"),
            Some(&serde_json::json!("test"))
        );
    }

    #[test]
    fn metadata_store_write_checkpoint_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        store.init_run("RUN3", b"{}", b"digraph {}").unwrap();

        let ctx = crate::context::Context::new();
        let cp1 = crate::checkpoint::Checkpoint::from_context(
            &ctx,
            "node_a",
            vec!["start".to_string()],
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            None,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );
        let cp1_json = serde_json::to_vec_pretty(&cp1).unwrap();
        store.write_checkpoint("RUN3", &cp1_json, &[]).unwrap();

        let cp2 = crate::checkpoint::Checkpoint::from_context(
            &ctx,
            "node_b",
            vec!["start".to_string(), "node_a".to_string()],
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            Some("node_c".to_string()),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );
        let cp2_json = serde_json::to_vec_pretty(&cp2).unwrap();
        store.write_checkpoint("RUN3", &cp2_json, &[]).unwrap();

        let loaded = MetadataStore::read_checkpoint(dir.path(), "RUN3")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.current_node, "node_b");
        assert_eq!(loaded.completed_nodes.len(), 2);
    }

    #[test]
    fn metadata_store_read_checkpoint_missing_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let result = MetadataStore::read_checkpoint(dir.path(), "NONEXISTENT").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn metadata_store_artifact_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        store.init_run("RUN4", b"{}", b"digraph {}").unwrap();

        let artifact_data = br#"{"large_output":"some data"}"#;
        let cp_json = b"{}"; // minimal checkpoint for the test
        store
            .write_checkpoint(
                "RUN4",
                cp_json,
                &[("artifacts/response.plan.json", artifact_data.as_slice())],
            )
            .unwrap();

        let read_back = MetadataStore::read_artifact(dir.path(), "RUN4", "response.plan")
            .unwrap()
            .unwrap();
        assert_eq!(read_back, artifact_data);
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
}
