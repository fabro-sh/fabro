//! Git operations on the run's actual workspace. Docker and Daytona execute
//! these commands inside the sandbox; the local provider uses its workspace.
//! No repository or Git bundle is kept on the server for a remote sandbox.
//! Checkpoint commits stay on the run branch, which the worker pushes after
//! each checkpoint. Forks fetch that branch from the origin.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use fabro_checkpoint::author::GitAuthor;
use fabro_checkpoint::trailer::{self, Trailer};
use fabro_store::platform_records::{DecisionRef, OperationKey};
use fabro_types::settings::run::{RunCheckpointSettings, RunNamespace};
use fabro_types::{DiffSummary, GitIdentitySource, SandboxProviderKind};
use petri_runtime::executor::{EnvError, ExecEnv, OutputMode, ProcessSpec, Sig};
use petri_runtime::ir::LogStream;
use tokio::process::Command;
use tokio::{fs, time};

use crate::diagnostics::CommandOutput;
use crate::source::{RunSource, SourceRevision};

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

/// How long a fetch from the run's origin may take.
const SOURCE_FETCH_TIMEOUT: Duration = Duration::from_mins(5);

/// Git's empty tree: what a root commit is diffed against.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

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

/// The settings a run's Git work runs under, as its namespace gives them:
/// who authors the checkpoint commits and where that identity came from,
/// the checkpoint settings, and whether the sandbox provider keeps the
/// workspaces on this host. The hooks and recovery both start from it.
#[derive(Clone, Debug)]
pub struct RunGitSettings {
    pub enabled:         bool,
    pub author:          GitAuthor,
    pub identity_source: GitIdentitySource,
    pub checkpoint:      RunCheckpointSettings,
    /// Whether the run's workspaces are on this host (the local sandbox
    /// provider). A run elsewhere snapshots inside its sandboxes.
    pub host_workspaces: bool,
}

impl From<&RunNamespace> for RunGitSettings {
    fn from(settings: &RunNamespace) -> Self {
        let author = settings
            .git
            .author
            .as_ref()
            .map(GitAuthor::from)
            .unwrap_or_default();
        let identity_source = if author.is_default() {
            GitIdentitySource::Default
        } else {
            GitIdentitySource::Explicit
        };
        Self {
            author,
            identity_source,
            enabled: settings.run_branch.enabled,
            checkpoint: settings.checkpoint.clone(),
            host_workspaces: settings.environment.provider == SandboxProviderKind::LOCAL,
        }
    }
}

impl Default for RunGitSettings {
    /// Fabro's default author and checkpoint settings, on this host.
    fn default() -> Self {
        Self {
            enabled:         true,
            author:          GitAuthor::default(),
            identity_source: GitIdentitySource::Default,
            checkpoint:      RunCheckpointSettings::default(),
            host_workspaces: true,
        }
    }
}

/// The identity of one snapshot: the attempt whose files it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckpointKey {
    pub execution: u64,
    pub firing:    u64,
    pub attempt:   u32,
}

impl CheckpointKey {
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

impl std::fmt::Display for CheckpointKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "execution {} firing {} attempt {}",
            self.execution, self.firing, self.attempt
        )
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
    #[error("git {action} failed ({status})")]
    Command {
        action: String,
        status: String,
        output: CommandOutput,
    },
    #[error("git {action}: {status}")]
    Sandbox {
        action: String,
        status: &'static str,
        #[source]
        source: EnvError,
    },
    #[error("the sandbox offered no byte stream for git {action}")]
    NoOutput { action: String },
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
    #[error("checkpoint {sha} is unavailable in the existing workspace")]
    MissingCommit { sha: String },
    #[error("fork recovery requires a Git repository source")]
    NoSource,
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

impl Site {
    /// Push an exact checkpoint from this workspace, with credentials scoped to
    /// this command. Nothing is persisted in Git configuration.
    pub async fn push(
        &self,
        url: &str,
        refspec: &str,
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<(), CheckpointError> {
        let output = RunWorkspaces::run_with(
            self,
            "push",
            &["push", "--quiet", url, refspec],
            env,
            timeout,
        )
        .await?;
        if output.success {
            Ok(())
        } else {
            Err(CheckpointError::Command {
                action: "push".to_owned(),
                status: "non-zero exit".to_owned(),
                output: CommandOutput::capture(&output.stdout, &output.stderr, env),
            })
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

/// The workspaces of one run on this host, and the Git operations Fabro
/// performs on them.
#[derive(Clone, Debug)]
pub struct RunWorkspaces {
    run_dir:       PathBuf,
    run_id:        String,
    author:        GitAuthor,
    exclude_globs: Vec<String>,
    timeout:       Duration,
    /// Where a Git target's workspaces are checked out from; `None` for a
    /// run with no remote repository.
    source:        Option<RunSource>,
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
            source: None,
        }
    }

    /// The same workspaces, checked out from `source` when a fresh run first
    /// acquires them, and restored from it into a fresh sandbox.
    #[must_use]
    pub fn with_source(mut self, source: Option<RunSource>) -> Self {
        self.source = source;
        self
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

    /// Whether the workspace exists on this host.
    pub async fn workspace_exists(&self, workspace: &str) -> bool {
        fs::try_exists(self.workspace_path(workspace))
            .await
            .unwrap_or(false)
    }

    /// The host site of a workspace: where `git` runs for a workspace kept
    /// on this host.
    #[must_use]
    pub fn host(&self, workspace: &str) -> Site {
        Site::Host(self.workspace_path(workspace))
    }

    /// Check the run's source out inside a fresh workspace. Existing
    /// repositories are left in place.
    pub async fn check_out_source(
        &self,
        site: &Site,
        _workspace: &str,
    ) -> Result<Option<String>, CheckpointError> {
        let Some(source) = &self.source else {
            return Ok(None);
        };
        if let Site::Host(path) = site {
            fs::create_dir_all(path)
                .await
                .map_err(|source| CheckpointError::Io {
                    path: path.clone(),
                    source,
                })?;
        }
        if self
            .git_status(site, "rev-parse", &["rev-parse", "--git-dir"])
            .await?
            .is_some()
        {
            return Ok(None);
        }
        self.git(site, "init", &["init", "-q"]).await?;
        self.git(site, "remote add", &[
            "remote",
            "add",
            "origin",
            &source.origin,
        ])
        .await?;
        self.fetch_source(source, site, "origin", &source.revision.refspec())
            .await?;
        self.git(site, "checkout", &[
            "checkout",
            "-q",
            "-B",
            &source.branch,
            "FETCH_HEAD",
        ])
        .await?;
        if let SourceRevision::Branch(branch) = &source.revision {
            // `origin/<branch>` resolves offline, as a clone leaves it.
            self.git(site, "update-ref", &[
                "update-ref",
                &format!("refs/remotes/origin/{branch}"),
                "FETCH_HEAD",
            ])
            .await?;
        }
        let sha = self.git(site, "rev-parse", &["rev-parse", "HEAD"]).await?;
        Ok(Some(sha))
    }

    /// Fetch `refspec` from `remote` (a remote name, or the origin's URL) at
    /// `site`, with `source`'s credential and depth.
    async fn fetch_source(
        &self,
        source: &RunSource,
        site: &Site,
        remote: &str,
        refspec: &str,
    ) -> Result<(), CheckpointError> {
        let depth = source.depth_arg();
        let mut args = vec!["fetch", "-q", "--no-tags"];
        if let Some(depth) = depth.as_deref() {
            args.push(depth);
        }
        args.extend([remote, "--", refspec]);
        let env = source.fetch_env().await;
        let output = Self::run_with(site, "fetch", &args, &env, SOURCE_FETCH_TIMEOUT).await?;
        if output.success {
            Ok(())
        } else {
            Err(CheckpointError::Command {
                action: "fetch".to_string(),
                status: "non-zero exit".to_string(),
                output: CommandOutput::capture(&output.stdout, &output.stderr, &env),
            })
        }
    }

    /// Whether the repository at `site` holds the commit `sha`.
    async fn has_object(&self, site: &Site, sha: &str) -> Result<bool, CheckpointError> {
        Ok(self
            .git_status(site, "cat-file", &[
                "cat-file",
                "-e",
                &format!("{sha}^{{commit}}"),
            ])
            .await?
            .is_some())
    }

    /// Commit the workspace's files on the run branch. Reuse an existing
    /// checkpoint of this run and attempt when the tree is unchanged.
    pub async fn commit(
        &self,
        site: &Site,
        workspace: &str,
        key: CheckpointKey,
        node: &str,
        status: &str,
    ) -> Result<Snapshot, CheckpointError> {
        if let Site::Host(path) = site {
            if !fs::try_exists(path).await.unwrap_or(false) {
                return Err(CheckpointError::WorkspaceMissing {
                    workspace: workspace.to_string(),
                    path:      path.clone(),
                });
            }
        }
        let branched = self.ensure_repository(site).await?;
        if let Some(existing) = self.find(site, key).await? {
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
        Ok(Snapshot {
            sha,
            reused: false,
            branched,
        })
    }

    /// The parent of a checkpoint commit, or `None` for a root commit.
    pub async fn commit_parent(
        &self,
        site: &Site,
        sha: &str,
    ) -> Result<Option<String>, CheckpointError> {
        self.git_status(site, "rev-parse", &[
            "rev-parse",
            "-q",
            "--verify",
            &format!("{sha}^"),
        ])
        .await
    }

    /// The diff from `base` (the empty tree when `None`) to `head`, both
    /// in the actual workspace.
    pub async fn diff(
        &self,
        site: &Site,
        base: Option<&str>,
        head: &str,
    ) -> Result<WorkspaceDiff, CheckpointError> {
        let base = base.unwrap_or(EMPTY_TREE);
        let numstat = self
            .git(site, "diff --numstat", &[
                "diff",
                "--numstat",
                "--no-color",
                base,
                head,
            ])
            .await?;
        let patch = self
            .git(site, "diff", &["diff", "--no-color", base, head])
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

    /// Find a checkpoint in this run's workspace by its commit trailers.
    pub async fn find(
        &self,
        site: &Site,
        key: CheckpointKey,
    ) -> Result<Option<String>, CheckpointError> {
        if self.head(site).await?.is_none() {
            return Ok(None);
        }
        let listed = self
            .git(site, "log", &[
                "log",
                "--format=%H",
                "--extended-regexp",
                &format!("--grep=^{RUN_TRAILER}: {}$", self.run_id),
                &format!("--grep=^{EXECUTION_TRAILER}: {}$", key.execution),
                &format!("--grep=^{FIRING_TRAILER}: {}$", key.firing),
                &format!("--grep=^{ATTEMPT_TRAILER}: {}$", key.attempt),
                "--all-match",
                "HEAD",
            ])
            .await?;
        Ok(listed.lines().next().map(str::to_owned))
    }

    /// Compare checkpoint ancestry inside the actual workspace.
    pub async fn is_ancestor(
        &self,
        site: &Site,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, CheckpointError> {
        Ok(self
            .git_status(site, "merge-base", &[
                "merge-base",
                "--is-ancestor",
                ancestor,
                descendant,
            ])
            .await?
            .is_some())
    }

    /// Whether the workspace sits on `sha` with nothing changed since.
    pub async fn matches(&self, site: &Site, sha: &str) -> Result<bool, CheckpointError> {
        Ok(self.head(site).await?.as_deref() == Some(sha) && self.is_clean(site).await?)
    }

    /// Whether the workspace's repository holds the commit `sha`, so a
    /// reset can reach it (without a transfer, in a sandbox); a directory
    /// that is gone or is no repository holds none.
    pub async fn has_commit(&self, site: &Site, sha: &str) -> Result<bool, CheckpointError> {
        if let Site::Host(path) = site {
            if !fs::try_exists(path).await.unwrap_or(false) {
                return Ok(false);
            }
        }
        if self
            .git_status(site, "rev-parse", &["rev-parse", "--git-dir"])
            .await?
            .is_none()
        {
            return Ok(false);
        }
        self.has_object(site, sha).await
    }

    /// Bring the workspace back to `sha`: tracked files reset, untracked
    /// files removed, the excluded caches left alone.
    pub async fn reset(&self, site: &Site, sha: &str) -> Result<(), CheckpointError> {
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

    /// Materialize an explicit fork from its source run's published branch.
    /// Fetch full branch history so any selected checkpoint remains reachable,
    /// even when the original run began with a depth-one checkout.
    pub async fn restore_fork(
        &self,
        site: &Site,
        source_run: &str,
        sha: &str,
    ) -> Result<(), CheckpointError> {
        let source = self.source.as_ref().ok_or(CheckpointError::NoSource)?;
        if let Site::Host(path) = site {
            fs::create_dir_all(path)
                .await
                .map_err(|source| CheckpointError::Io {
                    path: path.clone(),
                    source,
                })?;
        }
        self.git(site, "init", &["init", "-q"]).await?;
        let mut source = source.clone();
        source.depth = None;
        let branch = format!("refs/heads/fabro/run/{source_run}");
        self.fetch_source(&source, site, &source.origin, &branch)
            .await?;
        if !self.has_commit(site, sha).await? {
            return Err(CheckpointError::MissingCommit {
                sha: sha.to_owned(),
            });
        }
        self.git(site, "checkout", &[
            "checkout",
            "-q",
            "-B",
            &self.run_branch(),
            sha,
        ])
        .await?;
        // Keep the normal origin for tools running in the fork's workspace.
        self.git_status(site, "remote remove", &["remote", "remove", "origin"])
            .await?;
        self.git(site, "remote add", &[
            "remote",
            "add",
            "origin",
            &source.origin,
        ])
        .await?;
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

    /// The workspace's `HEAD`, or `None` when it has no commit.
    pub async fn head(&self, site: &Site) -> Result<Option<String>, CheckpointError> {
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
                output: CommandOutput::capture(&output.stdout, &output.stderr, &[]),
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
        Self::run_with(site, action, args, &[], self.timeout).await
    }

    /// [`Self::run`] with extra environment for the one command and its own
    /// deadline: a fetch from the origin carries its credential this way.
    async fn run_with<S: AsRef<str>>(
        site: &Site,
        action: &str,
        args: &[S],
        env: &[(String, String)],
        timeout: Duration,
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
            Site::Host(cwd) => Self::run_host(cwd, &all, action, env, timeout).await,
            Site::Sandbox(sandbox) => {
                Self::run_sandbox(sandbox, "git", &all, action, env, timeout).await
            }
        }
    }

    async fn run_host(
        cwd: &Path,
        args: &[&str],
        action: &str,
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<GitOutput, CheckpointError> {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(cwd)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        match time::timeout(timeout, command.output()).await {
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
                action: action.to_string(),
                timeout,
            }),
        }
    }

    /// Run `program` inside the sandbox, in its workspace, with the
    /// checkpoint's deadline on the process and both streams captured.
    async fn run_sandbox(
        env: &Arc<dyn ExecEnv>,
        program: &str,
        args: &[&str],
        action: &str,
        extra_env: &[(String, String)],
        timeout: Duration,
    ) -> Result<GitOutput, CheckpointError> {
        let spec = ProcessSpec::new(program, args)
            .with_output(OutputMode::Bytes)
            .with_timeout(Some(timeout))
            .with_env(
                extra_env
                    .iter()
                    .map(|(key, value)| (key.as_str().into(), value.as_str().into()))
                    .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
                    .collect(),
            );
        let mut handle = env
            .spawn(spec)
            .await
            .map_err(|error| CheckpointError::Sandbox {
                action: action.to_string(),
                status: "spawn failed",
                source: error,
            })?;
        let Some(mut chunks) = handle.bytes() else {
            let _ = handle.signal(Sig::Kill).await;
            let _ = handle.wait().await;
            return Err(CheckpointError::NoOutput {
                action: action.to_string(),
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
            .map_err(|error| CheckpointError::Sandbox {
                action: action.to_string(),
                status: "wait failed",
                source: error,
            })?;
        let (stdout, stderr) = drain.await.unwrap_or_default();
        if status.timed_out {
            return Err(CheckpointError::TimedOut {
                action: action.to_string(),
                timeout,
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
    async fn a_commit_is_found_reused_and_reset_in_the_existing_workspace() {
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
            .commit(
                &workspaces.host(workspace),
                workspace,
                key,
                "build",
                "success",
            )
            .await
            .expect("the commit");
        assert!(!first.reused);
        assert_eq!(
            first.branched,
            Some(BranchPoint { base_sha: None }),
            "the first commit created the run branch in a fresh repository"
        );
        let again = workspaces
            .commit(
                &workspaces.host(workspace),
                workspace,
                key,
                "build",
                "success",
            )
            .await
            .expect("the second commit");
        assert_eq!(again, Snapshot {
            sha:      first.sha.clone(),
            reused:   true,
            branched: None,
        });
        assert_eq!(
            workspaces
                .commit_parent(&workspaces.host(workspace), &first.sha)
                .await
                .expect("the parent lookup"),
            None
        );
        let diff = workspaces
            .diff(&workspaces.host(workspace), None, &first.sha)
            .await
            .expect("the diff from the empty tree");
        assert_eq!(diff.summary, DiffSummary {
            files_changed: 1,
            additions:     1,
            deletions:     0,
        });
        assert!(diff.patch.contains("+one"), "{}", diff.patch);
        assert_eq!(
            workspaces
                .find(&workspaces.host(workspace), key)
                .await
                .expect("the lookup"),
            Some(first.sha.clone())
        );
        assert!(
            workspaces
                .matches(&workspaces.host(workspace), &first.sha)
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
                .matches(&workspaces.host(workspace), &first.sha)
                .await
                .expect("matches")
        );
        workspaces
            .reset(&workspaces.host(workspace), &first.sha)
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
        assert!(
            !workspaces
                .has_commit(&workspaces.host(workspace), &first.sha)
                .await
                .unwrap()
        );
        assert!(!dir.path().join("snapshots").exists());
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
            .commit(
                &workspaces.host(workspace),
                workspace,
                key,
                "build",
                "success",
            )
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
            .commit(
                &workspaces.host(workspace),
                workspace,
                first_key,
                "start",
                "success",
            )
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
            .commit(
                &workspaces.host(workspace),
                workspace,
                second_key,
                "write",
                "success",
            )
            .await
            .expect("the second commit");
        assert_eq!(second.branched, None);
        assert_eq!(
            workspaces
                .commit_parent(&workspaces.host(workspace), &second.sha)
                .await
                .expect("the parent lookup"),
            Some(first.sha.clone())
        );
        let stage = workspaces
            .diff(&workspaces.host(workspace), Some(&first.sha), &second.sha)
            .await
            .expect("the stage diff");
        assert_eq!(stage.summary, DiffSummary {
            files_changed: 1,
            additions:     1,
            deletions:     0,
        });
        assert!(stage.patch.contains("+line 2"), "{}", stage.patch);
        let run = workspaces
            .diff(&workspaces.host(workspace), Some(&base), &second.sha)
            .await
            .expect("the run diff");
        assert_eq!(run.summary, stage.summary);
        let unchanged = workspaces
            .diff(&workspaces.host(workspace), Some(&base), &first.sha)
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
}
