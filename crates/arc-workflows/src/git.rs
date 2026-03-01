use std::path::Path;
use std::process::Command;

use arc_git_storage::branchstore::BranchStore;
use arc_git_storage::gitobj::Store;
use arc_git_storage::trailerlink::{self, Trailer};
use git2::{Repository, Signature};

use crate::checkpoint::Checkpoint;
use crate::error::{ArcError, Result};

fn git_error(msg: impl Into<String>) -> ArcError {
    ArcError::Engine(msg.into())
}

/// Assert the working directory is a clean git repo (no uncommitted changes).
pub fn ensure_clean(repo: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
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
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .map_err(|e| git_error(format!("git rev-parse failed: {e}")))?;

    if !output.status.success() {
        return Err(git_error("git rev-parse HEAD failed"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Create a new branch at HEAD without checking it out.
pub fn create_branch(repo: &Path, name: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["branch", name, "HEAD"])
        .current_dir(repo)
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
    let output = Command::new("git")
        .args(["worktree", "add"])
        .arg(path)
        .arg(branch)
        .current_dir(repo)
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
    let output = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .current_dir(repo)
        .output()
        .map_err(|e| git_error(format!("git worktree remove failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git worktree remove failed: {stderr}")));
    }

    Ok(())
}

/// Stage all changes and commit in `work_dir` with a structured message
/// including trailers for completed node count and shadow commit pointer.
/// Returns the new commit SHA.
pub fn checkpoint_commit(
    work_dir: &Path,
    run_id: &str,
    node_id: &str,
    status: &str,
    completed_count: usize,
    shadow_sha: Option<&str>,
) -> Result<String> {
    // Stage everything
    let output = Command::new("git")
        .args(["add", "-A"])
        .current_dir(work_dir)
        .output()
        .map_err(|e| git_error(format!("git add failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git add failed: {stderr}")));
    }

    // Build commit message with trailers
    let subject = format!("arc({run_id}): {node_id} ({status})");
    let completed_str = completed_count.to_string();
    let mut trailers = vec![
        Trailer {
            key: "Arc-Run",
            value: run_id,
        },
        Trailer {
            key: "Arc-Completed",
            value: &completed_str,
        },
    ];
    if let Some(sha) = shadow_sha {
        trailers.push(Trailer {
            key: "Arc-Checkpoint",
            value: sha,
        });
    }
    let message = trailerlink::format_message(&subject, "", &trailers);

    // Commit with arc identity (works even if user.name/email not configured)
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=arc",
            "-c",
            "user.email=arc@local",
            "commit",
            "--allow-empty",
            "-m",
            &message,
        ])
        .current_dir(work_dir)
        .output()
        .map_err(|e| git_error(format!("git commit failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git commit failed: {stderr}")));
    }

    head_sha(work_dir)
}

/// Compute the diff between a base commit and HEAD.
/// Returns the patch text (may be empty if no changes).
pub fn diff_against(work_dir: &Path, base: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", base, "HEAD"])
        .current_dir(work_dir)
        .output()
        .map_err(|e| git_error(format!("git diff failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git diff failed: {stderr}")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Git-native metadata storage for pipeline runs.
///
/// Stores checkpoint data, manifests, and graph DOT on an orphan branch
/// (`arc/{run_id}`) so that runs can be resumed from git alone.
pub struct MetadataStore {
    repo_path: std::path::PathBuf,
}

impl MetadataStore {
    pub fn new(repo_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            repo_path: repo_path.into(),
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
        let sig = Signature::now("arc", "arc@local")
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

    /// Read the manifest JSON from the metadata branch. Returns `None` if not found.
    pub fn read_manifest(repo_path: &Path, run_id: &str) -> Result<Option<serde_json::Value>> {
        match Self::read_file(repo_path, run_id, "manifest.json")? {
            Some(bytes) => {
                let val: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|e| git_error(format!("manifest deserialize failed: {e}")))?;
                Ok(Some(val))
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

    #[test]
    fn checkpoint_commit_creates_commit_with_trailers() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        create_branch(dir.path(), "run-branch").unwrap();

        let wt_path = dir.path().join("worktree");
        add_worktree(dir.path(), &wt_path, "run-branch").unwrap();

        // Write a file in the worktree
        fs::write(wt_path.join("output.txt"), "result").unwrap();

        // Simulate a shadow commit SHA
        let shadow_sha = "abcdef1234567890abcdef1234567890abcdef12";
        let sha =
            checkpoint_commit(&wt_path, "run1", "nodeA", "success", 3, Some(shadow_sha)).unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify commit message subject line
        let output = Command::new("git")
            .args(["log", "--oneline", "-1"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(log.contains("arc(run1): nodeA (success)"));

        // Verify trailers by reading full message (trim trailing newlines from git log)
        let output = Command::new("git")
            .args(["log", "--format=%B", "-1"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        let full_msg = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(trailerlink::parse(&full_msg, "Arc-Run"), Some("run1"));
        assert_eq!(trailerlink::parse(&full_msg, "Arc-Completed"), Some("3"));
        assert_eq!(
            trailerlink::parse(&full_msg, "Arc-Checkpoint"),
            Some(shadow_sha)
        );

        remove_worktree(dir.path(), &wt_path).unwrap();
    }

    #[test]
    fn checkpoint_commit_without_shadow_sha() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        create_branch(dir.path(), "run-branch2").unwrap();

        let wt_path = dir.path().join("worktree");
        add_worktree(dir.path(), &wt_path, "run-branch2").unwrap();

        let sha = checkpoint_commit(&wt_path, "run2", "nodeB", "completed", 1, None).unwrap();
        assert_eq!(sha.len(), 40);

        // Verify Arc-Completed trailer present but no Arc-Meta
        let output = Command::new("git")
            .args(["log", "--format=%B", "-1"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        let full_msg = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(trailerlink::parse(&full_msg, "Arc-Run"), Some("run2"));
        assert_eq!(trailerlink::parse(&full_msg, "Arc-Completed"), Some("1"));
        assert_eq!(trailerlink::parse(&full_msg, "Arc-Checkpoint"), None);

        remove_worktree(dir.path(), &wt_path).unwrap();
    }

    #[test]
    fn checkpoint_commit_with_no_user_config() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
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
            .current_dir(dir.path())
            .output()
            .unwrap();
        create_branch(dir.path(), "fallback-branch").unwrap();

        let wt_path = dir.path().join("worktree");
        add_worktree(dir.path(), &wt_path, "fallback-branch").unwrap();

        let sha = checkpoint_commit(&wt_path, "run2", "nodeB", "completed", 0, None).unwrap();
        assert_eq!(sha.len(), 40);

        remove_worktree(dir.path(), &wt_path).unwrap();
    }

    #[test]
    fn diff_against_shows_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let base = head_sha(dir.path()).unwrap();

        // Create a file and commit it
        fs::write(dir.path().join("new.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "-m",
                "add file",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let patch = diff_against(dir.path(), &base).unwrap();
        assert!(patch.contains("new.txt"));
        assert!(patch.contains("hello"));
    }

    #[test]
    fn diff_against_empty_when_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let base = head_sha(dir.path()).unwrap();
        let patch = diff_against(dir.path(), &base).unwrap();
        assert!(patch.is_empty());
    }

    // --- MetadataStore tests ---

    #[test]
    fn metadata_store_init_run_and_read() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let store = MetadataStore::new(dir.path());
        let manifest = br#"{"run_id":"RUN1","pipeline":"test"}"#;
        let dot = b"digraph { start -> end }";
        store.init_run("RUN1", manifest, dot).unwrap();

        let read_manifest = MetadataStore::read_manifest(dir.path(), "RUN1")
            .unwrap()
            .unwrap();
        assert_eq!(read_manifest["run_id"], "RUN1");
        assert_eq!(read_manifest["pipeline"], "test");

        let read_dot = MetadataStore::read_graph_dot(dir.path(), "RUN1")
            .unwrap()
            .unwrap();
        assert_eq!(read_dot, "digraph { start -> end }");
    }

    #[test]
    fn metadata_store_write_and_read_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let store = MetadataStore::new(dir.path());
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

        let store = MetadataStore::new(dir.path());
        store.init_run("RUN3", b"{}", b"digraph {}").unwrap();

        let ctx = crate::context::Context::new();
        let cp1 = crate::checkpoint::Checkpoint::from_context(
            &ctx,
            "node_a",
            vec!["start".to_string()],
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            None,
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

        let store = MetadataStore::new(dir.path());
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
}
