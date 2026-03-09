use std::path::Path;
use std::process::Command;

use arc_git_storage::branchstore::BranchStore;
use arc_git_storage::gitobj::Store;
use arc_git_storage::trailerlink::{self, Trailer};
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

/// Create a new branch pointing at a specific SHA (without checking it out).
pub fn create_branch_at(repo: &Path, name: &str, sha: &str) -> Result<()> {
    let output = git_cmd(repo)
        .args(["branch", "--force", name, sha])
        .output()
        .map_err(|e| git_error(format!("git branch failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git branch failed: {stderr}")));
    }

    Ok(())
}

/// Fast-forward the current branch to a given SHA.
/// Fails if the merge cannot be done as a fast-forward.
pub fn merge_ff_only(work_dir: &Path, sha: &str) -> Result<()> {
    let output = git_cmd(work_dir)
        .args(["merge", "--ff-only", sha])
        .output()
        .map_err(|e| git_error(format!("git merge --ff-only failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git merge --ff-only failed: {stderr}")));
    }

    Ok(())
}

/// Stage all changes and commit in `work_dir` with a structured message
/// including trailers for completed node count and shadow commit pointer.
/// Returns the new commit SHA.
#[allow(clippy::too_many_arguments)]
pub fn checkpoint_commit(
    work_dir: &Path,
    run_id: &str,
    node_id: &str,
    status: &str,
    completed_count: usize,
    shadow_sha: Option<&str>,
    excludes: &[String],
    author: &GitAuthor,
) -> Result<String> {
    tracing::debug!(path = %work_dir.display(), node_id, "Creating git checkpoint commit");
    // Stage everything (with optional excludes)
    let mut cmd = git_cmd(work_dir);
    cmd.args(["add", "-A", "--", "."]);
    for glob in excludes {
        cmd.arg(format!(":(glob,exclude){glob}"));
    }
    let output = cmd
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

    // Commit with configured identity (works even if user.name/email not configured)
    let name_cfg = format!("user.name={}", author.name);
    let email_cfg = format!("user.email={}", author.email);
    let output = git_cmd(work_dir)
        .args([
            "-c",
            &name_cfg,
            "-c",
            &email_cfg,
            "commit",
            "--allow-empty",
            "-m",
            &message,
        ])
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
    tracing::debug!(path = %work_dir.display(), "Computing git diff");
    let output = git_cmd(work_dir)
        .args(["diff", base, "HEAD"])
        .output()
        .map_err(|e| git_error(format!("git diff failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git diff failed: {stderr}")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Remove any stale worktree at `path` (best-effort), then add a fresh one.
pub fn replace_worktree(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    let _ = remove_worktree(repo, path);
    add_worktree(repo, path, branch)
}

/// Hard-reset the working directory to a specific SHA.
pub fn reset_hard(work_dir: &Path, sha: &str) -> Result<()> {
    let output = git_cmd(work_dir)
        .args(["reset", "--hard", sha])
        .output()
        .map_err(|e| git_error(format!("git reset --hard failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git reset --hard failed: {stderr}")));
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
    let output = git_cmd(repo)
        .args(["-c", "credential.helper=", "push", url, refname])
        .output()
        .map_err(|e| git_error(format!("git push failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git push failed: {stderr}")));
    }
    Ok(())
}

/// Push a local branch to the named remote using the user's configured credentials.
pub fn push_branch(repo: &Path, remote: &str, branch: &str) -> Result<()> {
    tracing::info!(
        repo_dir = %repo.display(),
        remote,
        branch,
        "Pushing branch to remote"
    );
    let output = git_cmd(repo)
        .args(["push", remote, branch])
        .output()
        .map_err(|e| git_error(format!("git push failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(git_error(format!("git push failed: {stderr}")));
    }
    Ok(())
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
    fn create_branch_at_specific_sha() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let initial_sha = head_sha(dir.path()).unwrap();

        // Make a second commit so HEAD differs from initial_sha
        fs::write(dir.path().join("f.txt"), "x").unwrap();
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
                "second",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Branch at the *initial* SHA (not HEAD)
        create_branch_at(dir.path(), "at-initial", &initial_sha).unwrap();

        let output = Command::new("git")
            .args(["rev-parse", "at-initial"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let branch_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(branch_sha, initial_sha);
    }

    #[test]
    fn merge_ff_only_advances_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let base_sha = head_sha(dir.path()).unwrap();

        // Create a branch and worktree, make a commit there
        create_branch(dir.path(), "ff-branch").unwrap();
        let wt = dir.path().join("ff-wt");
        add_worktree(dir.path(), &wt, "ff-branch").unwrap();
        fs::write(wt.join("new.txt"), "data").unwrap();
        checkpoint_commit(
            &wt,
            "run",
            "node",
            "ok",
            1,
            None,
            &[],
            &GitAuthor::default(),
        )
        .unwrap();
        let advanced_sha = head_sha(&wt).unwrap();
        remove_worktree(dir.path(), &wt).unwrap();

        // Main branch is still at base_sha
        assert_eq!(head_sha(dir.path()).unwrap(), base_sha);

        // Fast-forward main to advanced_sha
        merge_ff_only(dir.path(), &advanced_sha).unwrap();
        assert_eq!(head_sha(dir.path()).unwrap(), advanced_sha);
    }

    #[test]
    fn merge_ff_only_fails_on_diverged() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        // Create divergent history: commit on main
        fs::write(dir.path().join("a.txt"), "a").unwrap();
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
                "on main",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // A random SHA that isn't an ancestor/descendant
        let err = merge_ff_only(dir.path(), "0000000000000000000000000000000000000000");
        assert!(err.is_err());
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
        let sha = checkpoint_commit(
            &wt_path,
            "run1",
            "nodeA",
            "success",
            3,
            Some(shadow_sha),
            &[],
            &GitAuthor::default(),
        )
        .unwrap();
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

        let sha = checkpoint_commit(
            &wt_path,
            "run2",
            "nodeB",
            "completed",
            1,
            None,
            &[],
            &GitAuthor::default(),
        )
        .unwrap();
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

        let sha = checkpoint_commit(
            &wt_path,
            "run2",
            "nodeB",
            "completed",
            0,
            None,
            &[],
            &GitAuthor::default(),
        )
        .unwrap();
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
    fn reset_hard_resets_to_sha() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let initial_sha = head_sha(dir.path()).unwrap();

        // Make a commit
        fs::write(dir.path().join("file.txt"), "content").unwrap();
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
        assert_ne!(head_sha(dir.path()).unwrap(), initial_sha);

        // Reset back
        reset_hard(dir.path(), &initial_sha).unwrap();
        assert_eq!(head_sha(dir.path()).unwrap(), initial_sha);
        assert!(!dir.path().join("file.txt").exists());
    }

    #[test]
    fn checkpoint_commit_with_excludes_skips_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        create_branch(dir.path(), "excl-branch").unwrap();

        let wt_path = dir.path().join("worktree");
        add_worktree(dir.path(), &wt_path, "excl-branch").unwrap();

        // Create files: one should be staged, one excluded
        fs::write(wt_path.join("kept.txt"), "keep me").unwrap();
        fs::create_dir_all(wt_path.join("node_modules/pkg")).unwrap();
        fs::write(wt_path.join("node_modules/pkg/index.js"), "module").unwrap();

        let excludes = vec!["**/node_modules/**".to_string()];
        checkpoint_commit(
            &wt_path,
            "run",
            "node",
            "ok",
            1,
            None,
            &excludes,
            &GitAuthor::default(),
        )
        .unwrap();

        // Verify kept.txt was committed
        let output = Command::new("git")
            .args(["show", "--name-only", "--format=", "HEAD"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        let committed_files = String::from_utf8_lossy(&output.stdout);
        assert!(
            committed_files.contains("kept.txt"),
            "kept.txt should be committed"
        );
        assert!(
            !committed_files.contains("node_modules"),
            "node_modules should be excluded"
        );

        remove_worktree(dir.path(), &wt_path).unwrap();
    }

    #[test]
    fn checkpoint_commit_with_excludes_skips_modified_tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        create_branch(dir.path(), "excl-mod-branch").unwrap();

        let wt_path = dir.path().join("worktree");
        add_worktree(dir.path(), &wt_path, "excl-mod-branch").unwrap();

        // Create and commit a file in the excluded dir first
        fs::create_dir_all(wt_path.join(".cache")).unwrap();
        fs::write(wt_path.join(".cache/data.bin"), "v1").unwrap();
        checkpoint_commit(
            &wt_path,
            "run",
            "setup",
            "ok",
            0,
            None,
            &[],
            &GitAuthor::default(),
        )
        .unwrap();

        // Now modify the tracked excluded file and add a new non-excluded file
        fs::write(wt_path.join(".cache/data.bin"), "v2").unwrap();
        fs::write(wt_path.join("result.txt"), "done").unwrap();

        let excludes = vec!["**/.cache/**".to_string()];
        checkpoint_commit(
            &wt_path,
            "run",
            "step",
            "ok",
            1,
            None,
            &excludes,
            &GitAuthor::default(),
        )
        .unwrap();

        let output = Command::new("git")
            .args(["show", "--name-only", "--format=", "HEAD"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        let committed_files = String::from_utf8_lossy(&output.stdout);
        assert!(
            committed_files.contains("result.txt"),
            "result.txt should be committed"
        );
        assert!(
            !committed_files.contains(".cache"),
            ".cache should be excluded"
        );

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
}
