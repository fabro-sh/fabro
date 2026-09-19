//! Git snapshots of a Petri run's workspaces: the checkpoint commit and
//! what recovery does with it.
//!
//! A Fabro stage's files are committed on the run branch of its workspace
//! before the stage's finish is recorded, so a durable finish implies a
//! durable snapshot (the integration plan's F3.1). The commit message
//! carries the snapshot's identity as trailers, the run key, execution,
//! firing and attempt, so a restart reconciles a missing platform record
//! from the branch alone.
//!
//! # Where the workspace is
//!
//! Petri's host backend keeps a scope's workspace under the run directory
//! at `scopes/<workspace id>/work`, the layout `HostExecutor::workspace_for`
//! names. This module reaches it there and runs `git` on the host, which
//! is where the worker, and the server at recovery, run. A Docker or
//! Daytona workspace lives inside its sandbox: there `git` runs inside the
//! scope through the environment Petri hands the hooks at
//! `scope_acquired`, the same capability a step spawns its process with,
//! and the same commands run on both sites through one runner
//! ([`Site`]). Only the transfer differs: a sandbox commit leaves its
//! sandbox as a Git bundle and a restore enters one the same way.
//!
//! # The snapshot repository
//!
//! Every checkpoint commit is also published to a bare repository beside
//! the run's workspaces, `snapshots/<workspace id>.git`, under an immutable
//! ref per checkpoint (`refs/checkpoints/<execution>/<firing>/<attempt>`).
//! A host workspace pushes to it; a sandbox workspace bundles the commit
//! (`git bundle create`, against the newest ancestor the repository already
//! holds), the bundle is read out of the sandbox through the environment's
//! file transfer in parts the transport accepts, and the repository fetches
//! it. A workspace that is gone at recovery is restored from the
//! repository: a host directory fetches from it, a sandbox receives a
//! bundle of the checkpoint and fetches from that. The refs are what
//! recovery reconciles a missing record from, whatever the provider.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use fabro_checkpoint::author::GitAuthor;
use fabro_checkpoint::trailer::{self, Trailer};
use fabro_store::platform_records::{DecisionRef, OperationKey};
use fabro_types::DiffSummary;
use fabro_types::settings::run::RunCheckpointSettings;
use petri_runtime::executor::{EnvError, ExecEnv, OutputMode, ProcessSpec, Sig};
use petri_runtime::ir::LogStream;
use tokio::process::Command;
use tokio::{fs, time};

/// The failure class of a stage whose checkpoint commit failed: fatal to
/// the run, and terminal for a restart.
pub const CHECKPOINT_FAILED_CLASS: &str = "checkpoint_failed";

/// The effect kind of a checkpoint in its operation identity.
pub const CHECKPOINT_EFFECT: &str = "checkpoint";

pub const RUN_TRAILER: &str = "Fabro-Run";
pub const EXECUTION_TRAILER: &str = "Fabro-Execution";
pub const FIRING_TRAILER: &str = "Fabro-Firing";
pub const ATTEMPT_TRAILER: &str = "Fabro-Attempt";

const FOOTER: &str = "\u{2692}\u{fe0f} Generated with [Fabro](https://fabro.sh)";
const REFS_PREFIX: &str = "refs/checkpoints/";

/// Git's empty tree: what a root commit is diffed against.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Where a bundle waits inside a sandbox on its way in or out: outside the
/// workspace, so no checkpoint ever commits it.
const TRANSFER_DIR: &str = "/tmp/fabro-snapshots";
/// The largest piece of a bundle read out of a sandbox at once: half the
/// plugin transport's 16 MiB cap on one file read.
const TRANSFER_PART_BYTES: u64 = 8 * 1024 * 1024;

/// Directories never committed, the legacy executor's list: build output
/// and dependency caches a stage regenerates.
pub const EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".pnpm-store",
    ".npm",
    "target",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
    ".cache",
    ".tox",
    ".pytest_cache",
];

/// The identity of one snapshot: the attempt whose files it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckpointKey {
    pub execution: u64,
    pub firing:    u64,
    pub attempt:   u32,
}

impl CheckpointKey {
    /// The immutable ref the snapshot is published under.
    #[must_use]
    pub fn snapshot_ref(self) -> String {
        format!(
            "{REFS_PREFIX}{}/{}/{}",
            self.execution, self.firing, self.attempt
        )
    }

    /// The operation identity of the checkpoint effect: the attempt's
    /// decision in its execution, effect kind `checkpoint`.
    #[must_use]
    pub fn operation(self) -> OperationKey {
        self.operation_for(CHECKPOINT_EFFECT)
    }

    /// The operation identity of another effect performed for the same
    /// attempt, under `effect`.
    #[must_use]
    pub fn operation_for(self, effect: &str) -> OperationKey {
        OperationKey {
            execution: self.execution,
            decision:  DecisionRef::AttemptStart {
                firing:  self.firing,
                attempt: self.attempt,
            },
            effect:    effect.to_string(),
        }
    }

    /// The key an operation identity names, when it is a checkpoint's.
    #[must_use]
    pub fn from_operation(operation: &OperationKey) -> Option<Self> {
        match operation.decision {
            DecisionRef::AttemptStart { firing, attempt }
                if operation.effect == CHECKPOINT_EFFECT =>
            {
                Some(Self {
                    execution: operation.execution,
                    firing,
                    attempt,
                })
            }
            DecisionRef::AttemptStart { .. }
            | DecisionRef::ExecutionStart
            | DecisionRef::Route { .. } => None,
        }
    }

    /// The key as a file name fragment.
    fn transfer_name(self) -> String {
        format!("{}-{}-{}", self.execution, self.firing, self.attempt)
    }

    fn from_ref(name: &str) -> Option<Self> {
        let mut parts = name.strip_prefix(REFS_PREFIX)?.split('/');
        let execution = parts.next()?.parse().ok()?;
        let firing = parts.next()?.parse().ok()?;
        let attempt = parts.next()?.parse().ok()?;
        parts.next().is_none().then_some(Self {
            execution,
            firing,
            attempt,
        })
    }

    /// The key a checkpoint commit's message carries in its trailers.
    #[must_use]
    pub fn from_message(message: &str) -> Option<Self> {
        Some(Self {
            execution: trailer::parse(message, EXECUTION_TRAILER)?.parse().ok()?,
            firing:    trailer::parse(message, FIRING_TRAILER)?.parse().ok()?,
            attempt:   trailer::parse(message, ATTEMPT_TRAILER)?.parse().ok()?,
        })
    }
}

/// Why a snapshot could not be taken, found or restored.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("the workspace `{workspace}` does not exist at {}", path.display())]
    WorkspaceMissing {
        workspace: String,
        path:      PathBuf,
    },
    #[error("git {action} failed ({status}): {detail}")]
    Command {
        action: String,
        status: String,
        detail: String,
    },
    #[error("git {action} could not run")]
    Spawn {
        action: String,
        #[source]
        source: std::io::Error,
    },
    #[error("git {action} did not finish within {timeout:?}")]
    TimedOut { action: String, timeout: Duration },
    #[error("the workspace could not be prepared at {}", path.display())]
    Io {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the restored workspace is at {actual}, not the snapshot {expected}")]
    RestoreMismatch { expected: String, actual: String },
    #[error("the snapshot bundle could not be {action} the sandbox")]
    Transfer {
        /// `read out of` or `written into`.
        action: &'static str,
        #[source]
        source: EnvError,
    },
}

/// Where a workspace's `git` runs: in a directory on this host, or inside
/// a scope's sandbox through the environment Petri handed the hooks.
#[derive(Clone)]
pub enum Site {
    Host(PathBuf),
    Sandbox(Arc<dyn ExecEnv>),
}

impl std::fmt::Debug for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(path) => f.debug_tuple("Host").field(path).finish(),
            Self::Sandbox(env) => f
                .debug_tuple("Sandbox")
                .field(&env.workspace_path())
                .finish(),
        }
    }
}

/// What one `git` run produced, on either site.
struct GitOutput {
    success: bool,
    stdout:  Vec<u8>,
    stderr:  Vec<u8>,
}

/// A checkpoint commit: the commit, whether an earlier attempt of the same
/// operation had already made it, and, when this commit created the run
/// branch in its workspace, where the branch started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub sha:      String,
    pub reused:   bool,
    pub branched: Option<BranchPoint>,
}

/// Where a workspace's run branch was created: the commit the workspace
/// stood on, or `None` in a repository that had no commit yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchPoint {
    pub base_sha: Option<String>,
}

/// The difference between two snapshots: the summary `git diff --numstat`
/// gives and the patch itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceDiff {
    pub summary: DiffSummary,
    pub patch:   String,
}

impl WorkspaceDiff {
    /// Whether the two snapshots hold the same tree.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patch.trim().is_empty()
    }
}

/// One published snapshot of a workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedSnapshot {
    pub key: CheckpointKey,
    pub sha: String,
}

/// The workspaces of one run on this host, and the Git operations Fabro
/// performs on them.
#[derive(Clone, Debug)]
pub struct RunWorkspaces {
    run_dir:       PathBuf,
    run_id:        String,
    author:        GitAuthor,
    exclude_globs: Vec<String>,
    timeout:       Duration,
}

impl RunWorkspaces {
    #[must_use]
    pub fn new(
        run_dir: PathBuf,
        run_id: String,
        author: GitAuthor,
        settings: &RunCheckpointSettings,
    ) -> Self {
        Self {
            run_dir,
            run_id,
            author,
            exclude_globs: settings.exclude_globs.clone(),
            timeout: Duration::from_millis(settings.commit_timeout_ms.max(1)),
        }
    }

    /// The run branch every workspace of the run commits on.
    #[must_use]
    pub fn run_branch(&self) -> String {
        format!("fabro/run/{}", self.run_id)
    }

    /// Where the host backend keeps the workspace: `scopes/<id>/work` under
    /// the run directory.
    #[must_use]
    pub fn workspace_path(&self, workspace: &str) -> PathBuf {
        self.run_dir.join("scopes").join(workspace).join("work")
    }

    /// The bare repository the workspace's snapshots are published to.
    #[must_use]
    pub fn snapshot_repository(&self, workspace: &str) -> PathBuf {
        self.run_dir
            .join("snapshots")
            .join(format!("{workspace}.git"))
    }

    /// Whether the workspace exists on this host.
    pub async fn workspace_exists(&self, workspace: &str) -> bool {
        fs::try_exists(self.workspace_path(workspace))
            .await
            .unwrap_or(false)
    }

    /// The host site of a workspace.
    fn host(&self, workspace: &str) -> Site {
        Site::Host(self.workspace_path(workspace))
    }

    /// Commit the workspace's files on the run branch as the snapshot of
    /// `key`, and publish it. An earlier commit of the same key that the
    /// workspace still sits on, unchanged, is reused.
    pub async fn commit(
        &self,
        workspace: &str,
        key: CheckpointKey,
        node: &str,
        status: &str,
    ) -> Result<Snapshot, CheckpointError> {
        if !self.workspace_exists(workspace).await {
            return Err(CheckpointError::WorkspaceMissing {
                workspace: workspace.to_string(),
                path:      self.workspace_path(workspace),
            });
        }
        self.commit_at(&self.host(workspace), workspace, key, node, status)
            .await
    }

    /// [`commit`](Self::commit) for a workspace inside a sandbox: `git`
    /// runs in the scope through `env`, and the commit reaches the
    /// snapshot repository as a bundle.
    pub async fn commit_in(
        &self,
        env: &Arc<dyn ExecEnv>,
        workspace: &str,
        key: CheckpointKey,
        node: &str,
        status: &str,
    ) -> Result<Snapshot, CheckpointError> {
        self.commit_at(
            &Site::Sandbox(Arc::clone(env)),
            workspace,
            key,
            node,
            status,
        )
        .await
    }

    async fn commit_at(
        &self,
        site: &Site,
        workspace: &str,
        key: CheckpointKey,
        node: &str,
        status: &str,
    ) -> Result<Snapshot, CheckpointError> {
        let branched = self.ensure_repository(site).await?;
        if let Some(existing) = self.published_sha(workspace, key).await? {
            if self.head(site).await?.as_deref() == Some(existing.as_str())
                && self.is_clean(site).await?
            {
                return Ok(Snapshot {
                    sha: existing,
                    reused: true,
                    branched,
                });
            }
        }
        let mut add = vec![
            "add".to_string(),
            "-A".to_string(),
            "--".to_string(),
            ".".to_string(),
        ];
        add.extend(
            EXCLUDE_DIRS
                .iter()
                .map(|dir| format!(":(glob,exclude)**/{dir}/**")),
        );
        add.extend(
            self.exclude_globs
                .iter()
                .map(|glob| format!(":(glob,exclude){glob}")),
        );
        self.git(site, "add", &add).await?;
        let message = self.message(key, node, status);
        let user_name = format!("user.name={}", self.author.name);
        let user_email = format!("user.email={}", self.author.email);
        self.git(site, "commit", &[
            "-c",
            &user_name,
            "-c",
            &user_email,
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &message,
        ])
        .await?;
        let sha = self.git(site, "rev-parse", &["rev-parse", "HEAD"]).await?;
        match site {
            Site::Host(path) => self.publish(workspace, path, key, &sha).await?,
            Site::Sandbox(env) => self.publish_from_sandbox(env, workspace, key, &sha).await?,
        }
        Ok(Snapshot {
            sha,
            reused: false,
            branched,
        })
    }

    /// The parent of a published commit, or `None` for a root commit.
    pub async fn commit_parent(
        &self,
        workspace: &str,
        sha: &str,
    ) -> Result<Option<String>, CheckpointError> {
        let repository = Site::Host(self.ensure_snapshot_repository(workspace).await?);
        self.git_status(&repository, "rev-parse", &[
            "rev-parse",
            "-q",
            "--verify",
            &format!("{sha}^"),
        ])
        .await
    }

    /// The diff from `base` (the empty tree when `None`) to `head`, both
    /// published in the workspace's snapshot repository.
    pub async fn diff(
        &self,
        workspace: &str,
        base: Option<&str>,
        head: &str,
    ) -> Result<WorkspaceDiff, CheckpointError> {
        let repository = Site::Host(self.ensure_snapshot_repository(workspace).await?);
        let base = base.unwrap_or(EMPTY_TREE);
        let numstat = self
            .git(&repository, "diff --numstat", &[
                "diff",
                "--numstat",
                "--no-color",
                base,
                head,
            ])
            .await?;
        let patch = self
            .git(&repository, "diff", &["diff", "--no-color", base, head])
            .await?;
        let mut patch = patch;
        if !patch.is_empty() {
            patch.push('\n');
        }
        Ok(WorkspaceDiff {
            summary: numstat_summary(&numstat),
            patch,
        })
    }

    /// The commit of `key`, from the snapshot repository first, else from
    /// the host workspace's own history by the trailers.
    pub async fn find(
        &self,
        workspace: &str,
        key: CheckpointKey,
    ) -> Result<Option<String>, CheckpointError> {
        if let Some(sha) = self.published_sha(workspace, key).await? {
            return Ok(Some(sha));
        }
        let site = self.host(workspace);
        if !self.workspace_exists(workspace).await || self.head(&site).await?.is_none() {
            return Ok(None);
        }
        let listed = self
            .git(&site, "log", &[
                "log",
                "--format=%H",
                "--extended-regexp",
                &format!("--grep=^{EXECUTION_TRAILER}: {}$", key.execution),
                &format!("--grep=^{FIRING_TRAILER}: {}$", key.firing),
                &format!("--grep=^{ATTEMPT_TRAILER}: {}$", key.attempt),
                "--all-match",
                "HEAD",
            ])
            .await?;
        Ok(listed.lines().next().map(str::to_owned))
    }

    /// Every snapshot published for the workspace.
    pub async fn published(
        &self,
        workspace: &str,
    ) -> Result<Vec<PublishedSnapshot>, CheckpointError> {
        let repository = self.snapshot_repository(workspace);
        if !fs::try_exists(&repository).await.unwrap_or(false) {
            return Ok(Vec::new());
        }
        let listed = self
            .git(&Site::Host(repository), "for-each-ref", &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                REFS_PREFIX,
            ])
            .await?;
        Ok(listed
            .lines()
            .filter_map(|line| {
                let (name, sha) = line.split_once(' ')?;
                Some(PublishedSnapshot {
                    key: CheckpointKey::from_ref(name)?,
                    sha: sha.to_string(),
                })
            })
            .collect())
    }

    /// Whether `ancestor` is reachable from `descendant` in the workspace's
    /// published history.
    pub async fn is_ancestor(
        &self,
        workspace: &str,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, CheckpointError> {
        let repository = Site::Host(self.snapshot_repository(workspace));
        Ok(self
            .git_status(&repository, "merge-base", &[
                "merge-base",
                "--is-ancestor",
                ancestor,
                descendant,
            ])
            .await?
            .is_some())
    }

    /// The workspace's `HEAD`, or `None` when it has no commit.
    pub async fn workspace_head(&self, workspace: &str) -> Result<Option<String>, CheckpointError> {
        self.head(&self.host(workspace)).await
    }

    /// [`workspace_head`](Self::workspace_head) for a workspace inside a
    /// sandbox.
    pub async fn workspace_head_in(
        &self,
        env: &Arc<dyn ExecEnv>,
    ) -> Result<Option<String>, CheckpointError> {
        self.head(&Site::Sandbox(Arc::clone(env))).await
    }

    /// Whether the workspace sits on `sha` with nothing changed since.
    pub async fn matches(&self, workspace: &str, sha: &str) -> Result<bool, CheckpointError> {
        self.matches_at(&self.host(workspace), sha).await
    }

    /// [`matches`](Self::matches) for a workspace inside a sandbox.
    pub async fn matches_in(
        &self,
        env: &Arc<dyn ExecEnv>,
        sha: &str,
    ) -> Result<bool, CheckpointError> {
        self.matches_at(&Site::Sandbox(Arc::clone(env)), sha).await
    }

    async fn matches_at(&self, site: &Site, sha: &str) -> Result<bool, CheckpointError> {
        Ok(self.head(site).await?.as_deref() == Some(sha) && self.is_clean(site).await?)
    }

    /// Whether the host workspace's repository holds the commit `sha`, so a
    /// reset can reach it; a directory that is no repository holds none.
    pub async fn has_commit(&self, workspace: &str, sha: &str) -> Result<bool, CheckpointError> {
        if !self.workspace_exists(workspace).await {
            return Ok(false);
        }
        self.has_commit_at(&self.host(workspace), sha).await
    }

    /// Whether a sandbox workspace's repository holds the commit `sha`, so
    /// a reset can reach it without a transfer.
    pub async fn has_commit_in(
        &self,
        env: &Arc<dyn ExecEnv>,
        sha: &str,
    ) -> Result<bool, CheckpointError> {
        self.has_commit_at(&Site::Sandbox(Arc::clone(env)), sha)
            .await
    }

    async fn has_commit_at(&self, site: &Site, sha: &str) -> Result<bool, CheckpointError> {
        if self
            .git_status(site, "rev-parse", &["rev-parse", "--git-dir"])
            .await?
            .is_none()
        {
            return Ok(false);
        }
        Ok(self
            .git_status(site, "cat-file", &[
                "cat-file",
                "-e",
                &format!("{sha}^{{commit}}"),
            ])
            .await?
            .is_some())
    }

    /// Bring the workspace back to `sha`: tracked files reset, untracked
    /// files removed, the excluded caches left alone.
    pub async fn reset(&self, workspace: &str, sha: &str) -> Result<(), CheckpointError> {
        self.reset_at(&self.host(workspace), sha).await
    }

    /// [`reset`](Self::reset) for a workspace inside a sandbox.
    pub async fn reset_in(&self, env: &Arc<dyn ExecEnv>, sha: &str) -> Result<(), CheckpointError> {
        self.reset_at(&Site::Sandbox(Arc::clone(env)), sha).await
    }

    async fn reset_at(&self, site: &Site, sha: &str) -> Result<(), CheckpointError> {
        self.git(site, "reset", &["reset", "-q", "--hard", sha])
            .await?;
        let mut clean = vec!["clean".to_string(), "-fdq".to_string()];
        for dir in EXCLUDE_DIRS {
            clean.push("-e".to_string());
            clean.push((*dir).to_string());
        }
        for glob in &self.exclude_globs {
            clean.push("-e".to_string());
            clean.push(glob.clone());
        }
        self.git(site, "clean", &clean).await?;
        Ok(())
    }

    /// Recreate a gone workspace from the published snapshot `key`, at
    /// `sha`, on the run branch.
    pub async fn restore(
        &self,
        workspace: &str,
        key: CheckpointKey,
        sha: &str,
    ) -> Result<(), CheckpointError> {
        let path = self.workspace_path(workspace);
        fs::create_dir_all(&path)
            .await
            .map_err(|source| CheckpointError::Io {
                path: path.clone(),
                source,
            })?;
        let site = Site::Host(path);
        self.git(&site, "init", &["init", "-q"]).await?;
        let repository = self.snapshot_repository(workspace);
        let repository = repository.to_string_lossy().into_owned();
        self.git(&site, "fetch", &[
            "fetch",
            "-q",
            &repository,
            &key.snapshot_ref(),
        ])
        .await?;
        let branch = self.run_branch();
        self.git(&site, "checkout", &[
            "checkout",
            "-q",
            "-B",
            &branch,
            "FETCH_HEAD",
        ])
        .await?;
        self.verify_restored(&site, sha).await
    }

    /// [`restore`](Self::restore) into a sandbox: the snapshot enters the
    /// scope as a bundle of the checkpoint's ref, and the workspace, fresh
    /// or stale, is fetched from it and forced onto the run branch at `sha`.
    pub async fn restore_in(
        &self,
        env: &Arc<dyn ExecEnv>,
        workspace: &str,
        key: CheckpointKey,
        sha: &str,
    ) -> Result<(), CheckpointError> {
        let site = Site::Sandbox(Arc::clone(env));
        let repository = self.snapshot_repository(workspace);
        let bundle = self.transfer_path(&format!("restore-{}.bundle", key.transfer_name()));
        let staged = self.run_dir.join("snapshots").join(format!(
            "{workspace}.restore-{}.bundle",
            key.transfer_name()
        ));
        self.git(&Site::Host(repository), "bundle create", &[
            "bundle",
            "create",
            &staged.to_string_lossy(),
            &key.snapshot_ref(),
        ])
        .await?;
        let bytes = fs::read(&staged)
            .await
            .map_err(|source| CheckpointError::Io {
                path: staged.clone(),
                source,
            })?;
        let _ = fs::remove_file(&staged).await;
        env.write_file(Path::new(&bundle), &bytes)
            .await
            .map_err(|source| CheckpointError::Transfer {
                action: "written into",
                source,
            })?;
        let restored = async {
            self.git(&site, "init", &["init", "-q"]).await?;
            self.git(&site, "fetch", &[
                "fetch",
                "-q",
                &bundle,
                &key.snapshot_ref(),
            ])
            .await?;
            let branch = self.run_branch();
            self.git(&site, "checkout", &[
                "checkout",
                "-q",
                "-f",
                "-B",
                &branch,
                "FETCH_HEAD",
            ])
            .await?;
            self.reset_at(&site, "HEAD").await?;
            self.verify_restored(&site, sha).await
        }
        .await;
        self.remove_transfer(&site, &bundle).await;
        restored
    }

    async fn verify_restored(&self, site: &Site, sha: &str) -> Result<(), CheckpointError> {
        let actual = self.git(site, "rev-parse", &["rev-parse", "HEAD"]).await?;
        if actual != sha {
            return Err(CheckpointError::RestoreMismatch {
                expected: sha.to_string(),
                actual,
            });
        }
        Ok(())
    }

    /// The commit message: Fabro's subject, the footer, and the identity
    /// trailers last, so `git interpret-trailers` and
    /// [`CheckpointKey::from_message`] both read them.
    fn message(&self, key: CheckpointKey, node: &str, status: &str) -> String {
        let subject = format!("fabro({}): {node} ({status})", self.run_id);
        let execution = key.execution.to_string();
        let firing = key.firing.to_string();
        let attempt = key.attempt.to_string();
        let mut trailers = vec![
            Trailer {
                key:   RUN_TRAILER,
                value: &self.run_id,
            },
            Trailer {
                key:   EXECUTION_TRAILER,
                value: &execution,
            },
            Trailer {
                key:   FIRING_TRAILER,
                value: &firing,
            },
            Trailer {
                key:   ATTEMPT_TRAILER,
                value: &attempt,
            },
        ];
        let defaults = GitAuthor::default();
        let co_author = format!("{} <{}>", defaults.name, defaults.email);
        if !self.author.is_default() {
            trailers.push(Trailer {
                key:   "Co-Authored-By",
                value: &co_author,
            });
        }
        trailer::format_message(&subject, FOOTER, &trailers)
    }

    /// A repository on the run branch, initialised when the workspace has
    /// none. `Some` when the run branch was created here, with the commit
    /// the workspace stood on.
    async fn ensure_repository(&self, site: &Site) -> Result<Option<BranchPoint>, CheckpointError> {
        if self
            .git_status(site, "rev-parse", &["rev-parse", "--git-dir"])
            .await?
            .is_none()
        {
            self.git(site, "init", &["init", "-q"]).await?;
        }
        let branch = self.run_branch();
        let current = self
            .git_status(site, "symbolic-ref", &[
                "symbolic-ref",
                "-q",
                "--short",
                "HEAD",
            ])
            .await?;
        if current.as_deref() == Some(branch.as_str()) {
            return Ok(None);
        }
        let base_sha = self.head(site).await?;
        self.git(site, "checkout", &["checkout", "-q", "-B", &branch])
            .await?;
        Ok(Some(BranchPoint { base_sha }))
    }

    /// The bare snapshot repository of the workspace, created on first use.
    async fn ensure_snapshot_repository(
        &self,
        workspace: &str,
    ) -> Result<PathBuf, CheckpointError> {
        let repository = self.snapshot_repository(workspace);
        if !fs::try_exists(&repository).await.unwrap_or(false) {
            fs::create_dir_all(&repository)
                .await
                .map_err(|source| CheckpointError::Io {
                    path: repository.clone(),
                    source,
                })?;
            self.git(&Site::Host(repository.clone()), "init --bare", &[
                "init", "-q", "--bare",
            ])
            .await?;
        }
        Ok(repository)
    }

    /// Publish a host workspace's commit: a push into the snapshot
    /// repository.
    async fn publish(
        &self,
        workspace: &str,
        path: &Path,
        key: CheckpointKey,
        sha: &str,
    ) -> Result<(), CheckpointError> {
        let repository = self.ensure_snapshot_repository(workspace).await?;
        let refspec = format!("{sha}:{}", key.snapshot_ref());
        let repository = repository.to_string_lossy().into_owned();
        self.git(&Site::Host(path.to_path_buf()), "push", &[
            "push",
            "-q",
            "--force",
            &repository,
            &refspec,
        ])
        .await?;
        Ok(())
    }

    /// Publish a sandbox workspace's commit: a bundle of the run branch
    /// since the newest ancestor the snapshot repository already holds,
    /// read out of the sandbox in parts, fetched into the repository, and
    /// named there under the checkpoint's ref.
    async fn publish_from_sandbox(
        &self,
        env: &Arc<dyn ExecEnv>,
        workspace: &str,
        key: CheckpointKey,
        sha: &str,
    ) -> Result<(), CheckpointError> {
        let site = Site::Sandbox(Arc::clone(env));
        let repository = self.ensure_snapshot_repository(workspace).await?;
        let branch = format!("refs/heads/{}", self.run_branch());
        // The bundle carries only what the repository lacks when the
        // commit's parent is already there; the whole history otherwise.
        let parent = self
            .git_status(&site, "rev-parse", &[
                "rev-parse",
                "-q",
                "--verify",
                "HEAD~1",
            ])
            .await?;
        let basis = match parent {
            Some(parent)
                if self
                    .git_status(&Site::Host(repository.clone()), "cat-file", &[
                        "cat-file",
                        "-e",
                        &format!("{parent}^{{commit}}"),
                    ])
                    .await?
                    .is_some() =>
            {
                Some(parent)
            }
            _ => None,
        };
        let revision = match &basis {
            Some(parent) => format!("{parent}..{branch}"),
            None => branch.clone(),
        };
        let bundle = self.transfer_path(&format!("publish-{}.bundle", key.transfer_name()));
        let published = async {
            self.sh(&site, "prepare the transfer directory", &[
                "mkdir -p -- \"$(dirname -- \"$1\")\"",
                "sh",
                &bundle,
            ])
            .await?;
            self.git(&site, "bundle create", &[
                "bundle", "create", &bundle, &revision,
            ])
            .await?;
            let bytes = self.read_out(env, &site, &bundle).await?;
            let staged = self.run_dir.join("snapshots").join(format!(
                "{workspace}.publish-{}.bundle",
                key.transfer_name()
            ));
            fs::write(&staged, &bytes)
                .await
                .map_err(|source| CheckpointError::Io {
                    path: staged.clone(),
                    source,
                })?;
            let fetched = self
                .git(&Site::Host(repository.clone()), "fetch", &[
                    "fetch",
                    "-q",
                    &staged.to_string_lossy(),
                    &branch,
                ])
                .await;
            let _ = fs::remove_file(&staged).await;
            fetched?;
            self.git(&Site::Host(repository.clone()), "update-ref", &[
                "update-ref",
                &key.snapshot_ref(),
                sha,
            ])
            .await?;
            Ok(())
        }
        .await;
        self.remove_transfer(&site, &bundle).await;
        published
    }

    /// Where a transfer file of this run waits inside a sandbox.
    fn transfer_path(&self, name: &str) -> String {
        format!("{TRANSFER_DIR}/{}/{name}", self.run_id)
    }

    /// Read a file out of the sandbox in parts the transport accepts: the
    /// file is split beside itself, each part comes through the
    /// environment's file read, and the parts are removed as they go.
    async fn read_out(
        &self,
        env: &Arc<dyn ExecEnv>,
        site: &Site,
        path: &str,
    ) -> Result<Vec<u8>, CheckpointError> {
        let script = format!(
            "split -b {TRANSFER_PART_BYTES} -a 4 -- \"$1\" \"$1.part.\" && rm -f -- \"$1\" && ls \
             -1 -- \"$1\".part.*"
        );
        let listed = self
            .sh(site, "split the bundle", &[&script, "sh", path])
            .await?;
        let mut bytes = Vec::new();
        for part in listed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let read = env.read_file(Path::new(part)).await.map_err(|source| {
                CheckpointError::Transfer {
                    action: "read out of",
                    source,
                }
            })?;
            let Some(read) = read else {
                return Err(CheckpointError::Command {
                    action: "read the bundle".to_string(),
                    status: "missing".to_string(),
                    detail: format!("`{part}` is not in the sandbox"),
                });
            };
            bytes.extend(read);
            self.remove_transfer(site, part).await;
        }
        Ok(bytes)
    }

    /// Remove a transfer file from the sandbox, best effort.
    async fn remove_transfer(&self, site: &Site, path: &str) {
        let _ = self
            .sh(site, "remove the bundle", &["rm -f -- \"$1\"", "sh", path])
            .await;
    }

    /// Run a shell command in the sandbox; a non-zero exit is the error.
    async fn sh(
        &self,
        site: &Site,
        action: &str,
        args: &[&str],
    ) -> Result<String, CheckpointError> {
        let Site::Sandbox(env) = site else {
            return Err(CheckpointError::Command {
                action: action.to_string(),
                status: "no sandbox".to_string(),
                detail: "a shell transfer runs in a sandbox only".to_string(),
            });
        };
        let mut all = vec!["-c"];
        all.extend(args);
        let output = self.run_sandbox(env, "sh", &all, action).await?;
        if output.success {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(CheckpointError::Command {
                action: action.to_string(),
                status: "failed".to_string(),
                detail: detail(&output.stderr),
            })
        }
    }

    async fn published_sha(
        &self,
        workspace: &str,
        key: CheckpointKey,
    ) -> Result<Option<String>, CheckpointError> {
        let repository = self.snapshot_repository(workspace);
        if !fs::try_exists(&repository).await.unwrap_or(false) {
            return Ok(None);
        }
        self.git_status(&Site::Host(repository), "rev-parse", &[
            "rev-parse",
            "-q",
            "--verify",
            &key.snapshot_ref(),
        ])
        .await
    }

    async fn head(&self, site: &Site) -> Result<Option<String>, CheckpointError> {
        self.git_status(site, "rev-parse", &["rev-parse", "-q", "--verify", "HEAD"])
            .await
    }

    async fn is_clean(&self, site: &Site) -> Result<bool, CheckpointError> {
        let status = self.git(site, "status", &["status", "--porcelain"]).await?;
        Ok(status.trim().is_empty())
    }

    /// Run `git` at `site`; a non-zero exit is the error.
    async fn git<S: AsRef<str>>(
        &self,
        site: &Site,
        action: &str,
        args: &[S],
    ) -> Result<String, CheckpointError> {
        let output = self.run(site, action, args).await?;
        if output.success {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(CheckpointError::Command {
                action: action.to_string(),
                status: "non-zero exit".to_string(),
                detail: detail(&output.stderr),
            })
        }
    }

    /// Run `git` at `site`; a non-zero exit is `None`, for the queries whose
    /// answer it is (an unborn `HEAD`, a missing ref, no repository).
    async fn git_status<S: AsRef<str>>(
        &self,
        site: &Site,
        action: &str,
        args: &[S],
    ) -> Result<Option<String>, CheckpointError> {
        let output = self.run(site, action, args).await?;
        Ok(output
            .success
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }

    /// Run `git` with the arguments and the configuration every checkpoint
    /// command carries, at either site.
    async fn run<S: AsRef<str>>(
        &self,
        site: &Site,
        action: &str,
        args: &[S],
    ) -> Result<GitOutput, CheckpointError> {
        let mut all: Vec<&str> = vec![
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "gc.auto=0",
            "-c",
            "advice.detachedHead=false",
            "-c",
            "init.defaultBranch=main",
        ];
        all.extend(args.iter().map(AsRef::as_ref));
        match site {
            Site::Host(cwd) => self.run_host(cwd, &all, action).await,
            Site::Sandbox(env) => self.run_sandbox(env, "git", &all, action).await,
        }
    }

    async fn run_host(
        &self,
        cwd: &Path,
        args: &[&str],
        action: &str,
    ) -> Result<GitOutput, CheckpointError> {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        match time::timeout(self.timeout, command.output()).await {
            Ok(Ok(output)) => Ok(GitOutput {
                success: output.status.success(),
                stdout:  output.stdout,
                stderr:  output.stderr,
            }),
            Ok(Err(source)) => Err(CheckpointError::Spawn {
                action: action.to_string(),
                source,
            }),
            Err(_) => Err(CheckpointError::TimedOut {
                action:  action.to_string(),
                timeout: self.timeout,
            }),
        }
    }

    /// Run `program` inside the sandbox, in its workspace, with the
    /// checkpoint's deadline on the process and both streams captured.
    async fn run_sandbox(
        &self,
        env: &Arc<dyn ExecEnv>,
        program: &str,
        args: &[&str],
        action: &str,
    ) -> Result<GitOutput, CheckpointError> {
        let spec = ProcessSpec::new(program, args)
            .with_output(OutputMode::Bytes)
            .with_timeout(Some(self.timeout))
            .with_env(
                [("GIT_TERMINAL_PROMPT".into(), "0".into())]
                    .into_iter()
                    .collect(),
            );
        let mut handle = env
            .spawn(spec)
            .await
            .map_err(|error| CheckpointError::Command {
                action: action.to_string(),
                status: "spawn failed".to_string(),
                detail: error.to_string(),
            })?;
        let Some(mut chunks) = handle.bytes() else {
            let _ = handle.signal(Sig::Kill).await;
            let _ = handle.wait().await;
            return Err(CheckpointError::Command {
                action: action.to_string(),
                status: "no output stream".to_string(),
                detail: "the sandbox offered no byte stream for the command".to_string(),
            });
        };
        let drain = tokio::spawn(async move {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            while let Some(chunk) = chunks.recv().await {
                match chunk.stream {
                    LogStream::Stdout => stdout.extend(chunk.bytes),
                    LogStream::Stderr => stderr.extend(chunk.bytes),
                }
            }
            (stdout, stderr)
        });
        let status = handle
            .wait()
            .await
            .map_err(|error| CheckpointError::Command {
                action: action.to_string(),
                status: "wait failed".to_string(),
                detail: error.to_string(),
            })?;
        let (stdout, stderr) = drain.await.unwrap_or_default();
        if status.timed_out {
            return Err(CheckpointError::TimedOut {
                action:  action.to_string(),
                timeout: self.timeout,
            });
        }
        Ok(GitOutput {
            success: status.is_success(),
            stdout,
            stderr,
        })
    }
}

/// The summary `git diff --numstat` lines add up to: one line per file,
/// `<additions>\t<deletions>\t<path>`, with `-` for a binary file.
fn numstat_summary(numstat: &str) -> DiffSummary {
    let mut summary = DiffSummary::default();
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(additions), Some(deletions), Some(_path)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        summary.files_changed += 1;
        summary.additions += additions.parse::<i64>().unwrap_or(0);
        summary.deletions += deletions.parse::<i64>().unwrap_or(0);
    }
    summary
}

/// The tail of git's stderr for an error message: what the run's record
/// carries about the failure, bounded.
fn detail(stderr: &[u8]) -> String {
    const LIMIT: usize = 512;
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.is_empty() {
        return "no output".to_string();
    }
    let start = text.len().saturating_sub(LIMIT);
    let start = text
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= start)
        .unwrap_or(0);
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspaces(dir: &Path) -> RunWorkspaces {
        RunWorkspaces::new(
            dir.to_path_buf(),
            "run-1".to_string(),
            GitAuthor::default(),
            &RunCheckpointSettings::default(),
        )
    }

    #[test]
    fn a_key_round_trips_through_its_ref_and_its_operation() {
        let key = CheckpointKey {
            execution: 3,
            firing:    17,
            attempt:   2,
        };
        assert_eq!(key.snapshot_ref(), "refs/checkpoints/3/17/2");
        assert_eq!(CheckpointKey::from_ref(&key.snapshot_ref()), Some(key));
        assert_eq!(CheckpointKey::from_ref("refs/heads/main"), None);
        assert_eq!(CheckpointKey::from_operation(&key.operation()), Some(key));
        assert_eq!(
            CheckpointKey::from_operation(&OperationKey {
                execution: 3,
                decision:  DecisionRef::Route {
                    firing:  17,
                    attempt: 2,
                },
                effect:    CHECKPOINT_EFFECT.to_string(),
            }),
            None
        );
    }

    #[test]
    fn the_message_carries_the_identity_as_trailers_last() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let key = CheckpointKey {
            execution: 0,
            firing:    4,
            attempt:   1,
        };
        let message = workspaces(dir.path()).message(key, "build", "success");
        assert!(message.starts_with("fabro(run-1): build (success)\n\n"));
        assert_eq!(CheckpointKey::from_message(&message), Some(key));
        assert_eq!(trailer::parse(&message, RUN_TRAILER), Some("run-1"));
    }

    #[tokio::test]
    async fn a_commit_is_published_found_and_restored() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let workspaces = workspaces(dir.path());
        let workspace = "invocation-0-scope-0";
        let path = workspaces.workspace_path(workspace);
        fs::create_dir_all(&path).await.expect("the workspace");
        fs::write(path.join("out.txt"), "one\n")
            .await
            .expect("a file");
        let key = CheckpointKey {
            execution: 0,
            firing:    2,
            attempt:   1,
        };

        let first = workspaces
            .commit(workspace, key, "build", "success")
            .await
            .expect("the commit");
        assert!(!first.reused);
        assert_eq!(
            first.branched,
            Some(BranchPoint { base_sha: None }),
            "the first commit created the run branch in a fresh repository"
        );
        let again = workspaces
            .commit(workspace, key, "build", "success")
            .await
            .expect("the second commit");
        assert_eq!(again, Snapshot {
            sha:      first.sha.clone(),
            reused:   true,
            branched: None,
        });
        assert_eq!(
            workspaces
                .commit_parent(workspace, &first.sha)
                .await
                .expect("the parent lookup"),
            None
        );
        let diff = workspaces
            .diff(workspace, None, &first.sha)
            .await
            .expect("the diff from the empty tree");
        assert_eq!(diff.summary, DiffSummary {
            files_changed: 1,
            additions:     1,
            deletions:     0,
        });
        assert!(diff.patch.contains("+one"), "{}", diff.patch);
        assert_eq!(
            workspaces.find(workspace, key).await.expect("the lookup"),
            Some(first.sha.clone())
        );
        assert_eq!(
            workspaces.published(workspace).await.expect("the listing"),
            vec![PublishedSnapshot {
                key,
                sha: first.sha.clone(),
            }]
        );
        assert!(
            workspaces
                .matches(workspace, &first.sha)
                .await
                .expect("matches")
        );

        // The stage goes on, then the workspace is lost.
        fs::write(path.join("out.txt"), "two\n")
            .await
            .expect("a change");
        fs::write(path.join("scratch.txt"), "junk\n")
            .await
            .expect("an untracked file");
        assert!(
            !workspaces
                .matches(workspace, &first.sha)
                .await
                .expect("matches")
        );
        workspaces
            .reset(workspace, &first.sha)
            .await
            .expect("the reset");
        assert_eq!(
            fs::read_to_string(path.join("out.txt"))
                .await
                .expect("the file"),
            "one\n"
        );
        assert!(
            !fs::try_exists(path.join("scratch.txt"))
                .await
                .expect("exists")
        );

        fs::remove_dir_all(&path).await.expect("the workspace goes");
        assert_eq!(
            workspaces.find(workspace, key).await.expect("the lookup"),
            Some(first.sha.clone()),
            "the snapshot repository still knows the commit"
        );
        workspaces
            .restore(workspace, key, &first.sha)
            .await
            .expect("the restore");
        assert_eq!(
            fs::read_to_string(path.join("out.txt"))
                .await
                .expect("the restored file"),
            "one\n"
        );
        assert_eq!(
            workspaces
                .workspace_head(workspace)
                .await
                .expect("the head"),
            Some(first.sha)
        );
    }

    #[tokio::test]
    async fn an_unusable_repository_fails_the_commit() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let workspaces = workspaces(dir.path());
        let workspace = "invocation-0-scope-0";
        let path = workspaces.workspace_path(workspace);
        fs::create_dir_all(&path).await.expect("the workspace");
        fs::write(path.join(".git"), "garbage\n")
            .await
            .expect("a broken gitfile");
        let key = CheckpointKey {
            execution: 0,
            firing:    2,
            attempt:   1,
        };
        let error = workspaces
            .commit(workspace, key, "build", "success")
            .await
            .expect_err("the commit fails");
        assert!(matches!(error, CheckpointError::Command { .. }), "{error}");
    }

    #[tokio::test]
    async fn a_second_commit_diffs_from_its_parent_and_a_branch_from_its_base() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let workspaces = workspaces(dir.path());
        let workspace = "invocation-0-scope-0";
        let path = workspaces.workspace_path(workspace);
        fs::create_dir_all(&path).await.expect("the workspace");
        fs::write(path.join("story.txt"), "line 1\n")
            .await
            .expect("a file");
        // A source repository with a commit: the run branch starts from it.
        for args in [vec!["init", "-q"], vec!["add", "."], vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "initial",
        ]] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(&path)
                .status()
                .await
                .expect("git runs");
            assert!(status.success(), "git {args:?}");
        }
        let base = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&path)
                .output()
                .await
                .expect("git runs")
                .stdout,
        )
        .expect("utf-8")
        .trim()
        .to_string();

        let first_key = CheckpointKey {
            execution: 0,
            firing:    1,
            attempt:   1,
        };
        let first = workspaces
            .commit(workspace, first_key, "start", "success")
            .await
            .expect("the first commit");
        assert_eq!(
            first.branched,
            Some(BranchPoint {
                base_sha: Some(base.clone()),
            })
        );
        fs::write(path.join("story.txt"), "line 1\nline 2\n")
            .await
            .expect("a change");
        let second_key = CheckpointKey {
            execution: 0,
            firing:    2,
            attempt:   1,
        };
        let second = workspaces
            .commit(workspace, second_key, "write", "success")
            .await
            .expect("the second commit");
        assert_eq!(second.branched, None);
        assert_eq!(
            workspaces
                .commit_parent(workspace, &second.sha)
                .await
                .expect("the parent lookup"),
            Some(first.sha.clone())
        );
        let stage = workspaces
            .diff(workspace, Some(&first.sha), &second.sha)
            .await
            .expect("the stage diff");
        assert_eq!(stage.summary, DiffSummary {
            files_changed: 1,
            additions:     1,
            deletions:     0,
        });
        assert!(stage.patch.contains("+line 2"), "{}", stage.patch);
        let run = workspaces
            .diff(workspace, Some(&base), &second.sha)
            .await
            .expect("the run diff");
        assert_eq!(run.summary, stage.summary);
        let unchanged = workspaces
            .diff(workspace, Some(&base), &first.sha)
            .await
            .expect("the empty diff");
        assert!(unchanged.is_empty());
        assert_eq!(unchanged.summary, DiffSummary::default());
    }

    #[test]
    fn numstat_lines_add_up_and_binary_files_count_as_changed() {
        assert_eq!(
            numstat_summary("3\t1\ta.txt\n-\t-\timage.png\n"),
            DiffSummary {
                files_changed: 2,
                additions:     3,
                deletions:     1,
            }
        );
        assert_eq!(numstat_summary(""), DiffSummary::default());
    }

    #[test]
    fn detail_keeps_the_tail_of_long_output() {
        let long = "x".repeat(600);
        assert_eq!(detail(long.as_bytes()).len(), 512);
        assert_eq!(detail(b""), "no output");
    }
}
