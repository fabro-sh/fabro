//! Native Git acquisition owned by the CLI, with no server credential lookup.
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context as _, bail};
use fabro_manifest::CollectedWorkflowClosure;
use fabro_proc::ProcessError;
use fabro_types::{GitHubHost, GitHubRepositorySlug, GitRunTarget, repository};
use tokio::process::Command;
use tokio::{fs, signal as tokio_signal, task};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::selection::RemoteWorkflowRevision;
use crate::args::RunArgs;

const OUTPUT_LIMIT: usize = 64 * 1024;

/// Configuration overrides that keep an untrusted checkout from running code
/// or rewriting bytes: no hooks or fsmonitor, no LFS smudge, no submodule
/// recursion, no `ext::` transport, no background maintenance, no line-ending
/// conversion.
const HARDENED_GIT_CONFIG: &[&str] = &[
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "filter.lfs.smudge=",
    "-c",
    "filter.lfs.process=",
    "-c",
    "filter.lfs.required=false",
    "-c",
    "submodule.recurse=false",
    "-c",
    "protocol.ext.allow=never",
    "-c",
    "maintenance.auto=0",
    "-c",
    "gc.auto=0",
    "-c",
    "core.autocrlf=false",
];

#[derive(Debug, thiserror::Error)]
pub(super) enum RemoteWorkflowError {
    #[error(
        "local Git {operation} failed ({status}); verify native Git access to the repository using your local credential helper or SSH configuration; Fabro server login does not grant Git access"
    )]
    Process {
        operation: &'static str,
        status:    ExitStatus,
    },
    #[error("local Git command timed out")]
    Timeout,
    #[error("local Git acquisition cancelled")]
    Cancelled,
    #[error("local Git metadata exceeds the 64 KiB capture limit")]
    OutputLimit,
    #[error("local Git I/O failed")]
    Io(#[from] std::io::Error),
}

/// Every command runs inside a scratch repository Fabro owns, never the
/// caller's working directory, so metadata lookup, fetch, and checkout all see
/// the same Git configuration: the user's global and system config applies,
/// repository-local config from wherever the CLI was invoked does not.
pub(super) struct NativeGit {
    host:                   GitHubHost,
    timeout:                Duration,
    #[cfg(test)]
    pub(super) environment: Vec<(String, String)>,
}

impl NativeGit {
    pub(super) fn new(host: GitHubHost) -> Self {
        Self {
            host,
            timeout: Duration::from_mins(2),
            #[cfg(test)]
            environment: Vec::new(),
        }
    }

    /// Run one Git command; `operation` labels it in failure diagnostics.
    async fn command(
        &self,
        operation: &'static str,
        cwd: &Path,
        args: &[&str],
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, RemoteWorkflowError> {
        if cancel.is_cancelled() {
            return Err(RemoteWorkflowError::Cancelled);
        }
        let mut command = Command::new("git");
        command
            .stdin(Stdio::null())
            .current_dir(cwd)
            .args(HARDENED_GIT_CONFIG)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
        #[cfg(test)]
        command.envs(self.environment.iter().cloned());
        let output =
            fabro_proc::capture(&mut command, Some(self.timeout), cancel, Some(OUTPUT_LIMIT))
                .await
                .map_err(|error| match error {
                    ProcessError::TimedOut => RemoteWorkflowError::Timeout,
                    ProcessError::Cancelled => RemoteWorkflowError::Cancelled,
                    ProcessError::Io(source) => RemoteWorkflowError::Io(source),
                })?;
        if !output.output.status.success() {
            // Output may contain arbitrary helper/config secrets, even after pattern
            // redaction. Never retain it in an error/cause chain or tracing event.
            return Err(RemoteWorkflowError::Process {
                operation,
                status: output.output.status,
            });
        }
        if output.stdout_truncated {
            return Err(RemoteWorkflowError::OutputLimit);
        }
        Ok(output.output.stdout)
    }

    /// Create and initialize an empty scratch repository. Temporary files are
    /// removed when the returned directory drops.
    async fn scratch_repository(
        &self,
        cancel: &CancellationToken,
    ) -> anyhow::Result<tempfile::TempDir> {
        let scratch = tempfile::Builder::new()
            .prefix("fabro-workflow-")
            .tempdir()?;
        self.command(
            "repository initialization",
            scratch.path(),
            &["init", "--quiet", "--template="],
            cancel,
        )
        .await?;
        Ok(scratch)
    }

    async fn records(
        &self,
        root: &Path,
        repository: &GitHubRepositorySlug,
        patterns: &[String],
        cancel: &CancellationToken,
    ) -> anyhow::Result<String> {
        let url = repository.https_url(&self.host);
        let mut args = vec!["ls-remote", "--symref", &url];
        args.extend(patterns.iter().map(String::as_str));
        let bytes = self.command("metadata lookup", root, &args, cancel).await?;
        Ok(std::str::from_utf8(&bytes)
            .context("local Git returned invalid metadata encoding")?
            .to_owned())
    }

    pub(super) async fn resolve_target(
        &self,
        repository: GitHubRepositorySlug,
        branch: Option<String>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<GitRunTarget> {
        let scratch = self.scratch_repository(cancel).await?;
        let root = scratch.path();
        let (branch, sha) = if let Some(branch) = branch {
            let reference = format!("refs/heads/{branch}");
            let records = self
                .records(root, &repository, std::slice::from_ref(&reference), cancel)
                .await?;
            let sha = exact_record(&records, &reference)?.context("target branch was not found")?;
            (branch, sha)
        } else {
            let records = self
                .records(root, &repository, &["HEAD".into()], cancel)
                .await?;
            default_target_branch(&records)?
        };
        let target = GitRunTarget {
            repo: repository.to_string(),
            branch,
            tag: None,
            sha: Some(sha),
        };
        Ok(target.validate(&self.host)?.into_target())
    }

    /// Resolve `revision` to a commit SHA using metadata lookups run from
    /// `root`, an initialized scratch repository.
    async fn resolve_revision(
        &self,
        root: &Path,
        repository: &GitHubRepositorySlug,
        revision: &RemoteWorkflowRevision,
        cancel: &CancellationToken,
    ) -> anyhow::Result<String> {
        match revision {
            RemoteWorkflowRevision::Commit(sha) => Ok(sha.clone()),
            RemoteWorkflowRevision::DefaultBranch => {
                let records = self
                    .records(root, repository, &["HEAD".into()], cancel)
                    .await?;
                Ok(default_head(&records)?.1)
            }
            RemoteWorkflowRevision::Branch(reference) => {
                let records = self
                    .records(root, repository, std::slice::from_ref(reference), cancel)
                    .await?;
                exact_record(&records, reference)?.context(REF_NOT_FOUND)
            }
            RemoteWorkflowRevision::Tag(reference) => {
                let records = self
                    .records(root, repository, &tag_patterns(reference), cancel)
                    .await?;
                tag_commit(&records, reference)?.context(REF_NOT_FOUND)
            }
            RemoteWorkflowRevision::Name(name) => {
                let branch = format!("refs/heads/{name}");
                let tag = format!("refs/tags/{name}");
                let mut patterns = vec![branch.clone()];
                patterns.extend(tag_patterns(&tag));
                let records = self.records(root, repository, &patterns, cancel).await?;
                resolve_name(&records, &branch, &tag)
            }
        }
    }

    pub(super) async fn collect(
        &self,
        repository: GitHubRepositorySlug,
        selector: PathBuf,
        revision: RemoteWorkflowRevision,
        cancel: CancellationToken,
    ) -> anyhow::Result<CollectedWorkflowClosure> {
        let checkout = self.scratch_repository(&cancel).await?;
        let sha = self
            .resolve_revision(checkout.path(), &repository, &revision, &cancel)
            .await?;
        self.collect_checkout(repository, selector, sha, checkout, cancel)
            .await
    }

    async fn collect_checkout(
        &self,
        repository: GitHubRepositorySlug,
        selector: PathBuf,
        sha: String,
        checkout: tempfile::TempDir,
        cancel: CancellationToken,
    ) -> anyhow::Result<CollectedWorkflowClosure> {
        self.checkout(&repository, &sha, checkout.path(), &cancel)
            .await?;
        // Moving the directory into the blocking task keeps it alive even if
        // the outer future is dropped. Collection is not interruptible; finish
        // it and clean up before reporting cancellation.
        let collection_cancel = cancel.clone();
        let closure = task::spawn_blocking(move || {
            if collection_cancel.is_cancelled() {
                return Err(RemoteWorkflowError::Cancelled.into());
            }
            fabro_manifest::collect_workflow_versions(&selector, checkout.path())
                .map_err(anyhow::Error::new)
        })
        .await
        .context("workflow collection task failed")??;
        if cancel.is_cancelled() {
            return Err(RemoteWorkflowError::Cancelled.into());
        }
        Ok(closure)
    }

    /// Fetch and check out `sha` into `root`, an initialized scratch
    /// repository.
    async fn checkout(
        &self,
        repository: &GitHubRepositorySlug,
        sha: &str,
        root: &Path,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        self.command(
            "fetch",
            root,
            &[
                "fetch",
                "--quiet",
                "--depth=1",
                "--no-tags",
                "--no-recurse-submodules",
                &repository.https_url(&self.host),
                sha,
            ],
            cancel,
        )
        .await?;
        let kind = self
            .command("object inspection", root, &["cat-file", "-t", sha], cancel)
            .await?;
        if kind != b"commit\n" {
            bail!("the selected source SHA is not a commit");
        }
        // The highest-precedence attributes file prevents repository attributes
        // from invoking configured filters or rewriting the committed source bytes.
        fs::create_dir_all(root.join(".git/info")).await?;
        fs::write(
            root.join(".git/info/attributes"),
            "* -filter -text -ident -working-tree-encoding\n",
        )
        .await?;
        self.command(
            "checkout",
            root,
            &[
                "-c",
                &format!("core.worktree={}", root.display()),
                "checkout",
                "--quiet",
                "--detach",
                sha,
                "--",
            ],
            cancel,
        )
        .await?;
        let head = self
            .command(
                "checkout verification",
                root,
                &["rev-parse", "--verify", "HEAD"],
                cancel,
            )
            .await?;
        if head != format!("{sha}\n").as_bytes() {
            bail!("workflow checkout did not match the selected commit");
        }
        Ok(())
    }
}

fn exact_record(records: &str, reference: &str) -> anyhow::Result<Option<String>> {
    let mut found = None;
    for line in records.lines() {
        let Some((value, name)) = line.split_once('\t') else {
            continue;
        };
        if name != reference || value.starts_with("ref: ") {
            continue;
        }
        let sha = repository::normalize_git_commit_sha(value)
            .context("invalid Git metadata commit SHA")?;
        if found.as_ref().is_some_and(|previous| previous != &sha) {
            bail!("conflicting Git metadata records");
        }
        found = Some(sha);
    }
    Ok(found)
}

fn default_head(records: &str) -> anyhow::Result<(String, String)> {
    let mut branch = None;
    for line in records.lines() {
        if let Some(value) = line
            .strip_prefix("ref: refs/heads/")
            .and_then(|line| line.strip_suffix("\tHEAD"))
        {
            if !repository::is_valid_github_ref_selector(value) {
                bail!("remote default HEAD does not name a valid branch");
            }
            if branch.is_some() {
                bail!("ambiguous remote default HEAD");
            }
            branch = Some(value.to_owned());
        }
    }
    Ok((
        branch.context("remote default HEAD must name a branch")?,
        exact_record(records, "HEAD")?.context("remote default HEAD has no commit")?,
    ))
}

/// The remote default branch as a run target. `GitRunTarget` requires a bare
/// working branch name, which is stricter than the ref grammar `default_head`
/// accepts for workflow acquisition; report the mismatch with the flag that
/// resolves it instead of a generic branch-grammar error.
fn default_target_branch(records: &str) -> anyhow::Result<(String, String)> {
    let (branch, sha) = default_head(records)?;
    if !repository::is_valid_git_branch_name(&branch) {
        bail!(
            "remote default branch `{branch}` cannot name a run target branch; pass --target-branch to select a working branch"
        );
    }
    Ok((branch, sha))
}

const REF_NOT_FOUND: &str = "workflow ref was not found; no alternative revision was selected";

/// `ls-remote` patterns for a tag: the tag itself and its peeled commit.
fn tag_patterns(tag: &str) -> [String; 2] {
    [tag.to_owned(), format!("{tag}^{{}}")]
}

/// The commit a tag names, preferring the peeled commit of an annotated tag
/// over the tag object.
fn tag_commit(records: &str, tag: &str) -> anyhow::Result<Option<String>> {
    let Some(sha) = exact_record(records, tag)? else {
        return Ok(None);
    };
    Ok(Some(
        exact_record(records, &format!("{tag}^{{}}"))?.unwrap_or(sha),
    ))
}

/// A bare name may be a branch or a tag; it must be exactly one.
fn resolve_name(records: &str, branch: &str, tag: &str) -> anyhow::Result<String> {
    match (exact_record(records, branch)?, tag_commit(records, tag)?) {
        (Some(_), Some(_)) => bail!(
            "workflow ref is ambiguous between a branch and tag; use refs/heads/... or refs/tags/..."
        ),
        (Some(sha), None) | (None, Some(sha)) => Ok(sha),
        (None, None) => bail!(REF_NOT_FOUND),
    }
}

/// Cooperative Ctrl-C handling for commands that acquire sources with native
/// Git.
///
/// Tokio's Ctrl-C listener permanently replaces the default SIGINT disposition
/// for the process, so it is installed only when native Git is in play, and the
/// command keeps it armed for every phase up to the point where `attach`
/// installs its own listener or the process exits. Interruption cancels owned
/// Git tasks and waits for their cleanup; an in-progress blocking collection
/// must finish first.
#[derive(Clone)]
pub(crate) struct Interruption {
    cancel:  CancellationToken,
    tasks:   TaskTracker,
    listens: bool,
}

impl Interruption {
    /// `native_git` reports whether any selection runs native Git. Without it
    /// the default SIGINT disposition is left untouched and `guard` is a
    /// pass-through.
    pub(crate) fn new(native_git: bool) -> Self {
        Self {
            cancel:  CancellationToken::new(),
            tasks:   TaskTracker::new(),
            listens: native_git,
        }
    }

    pub(crate) fn for_run_args(args: &RunArgs) -> Self {
        Self::new(
            args.workflow_repo.is_some()
                || args.target_repo.is_some()
                || args.target_repo_selector.is_some()
                || args
                    .workflow
                    .as_deref()
                    .is_some_and(|path| super::selection::workflow_shorthand(path).is_some()),
        )
    }

    /// Run `work` to completion, or until Ctrl-C cancels it and every owned
    /// Git task has cleaned up.
    pub(crate) async fn guard<T>(
        &self,
        work: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        if !self.listens {
            return work.await;
        }
        tokio::select! {
            result = work => result,
            signal = tokio_signal::ctrl_c() => {
                self.cancel.cancel();
                self.tasks.close();
                self.tasks.wait().await;
                signal.context("failed to listen for interruption")?;
                Err(RemoteWorkflowError::Cancelled.into())
            }
        }
    }

    /// The task owns its child processes and temporary checkout. Dropping the
    /// waiter requests cooperative cleanup, not task abortion; the task keeps
    /// running until its Git children are reaped and its files are removed.
    pub(super) async fn owned<T: Send + 'static, TFuture>(
        &self,
        work: impl FnOnce(CancellationToken) -> TFuture + Send + 'static,
    ) -> anyhow::Result<T>
    where
        TFuture: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let cancel = self.cancel.child_token();
        let _cancel_on_drop = cancel.clone().drop_guard();
        self.tasks
            .spawn(work(cancel.clone()))
            .await
            .context("local Git task failed")?
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "hermetic Git fixtures and fake executables use synchronous file setup"
)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use nix::sys::signal::{self, Signal};
    use nix::sys::stat::Mode;
    use nix::unistd;
    use tokio::time;

    use super::super::test_support::{commit_all, write_workflow};
    use super::*;

    #[test]
    fn shorthand_acquisition_owns_interruption_but_local_paths_do_not() {
        for (flags, listens) in [
            (vec!["acme/workflows:review"], true),
            (vec!["review", "--target", "acme/app@main"], true),
            (vec!["./acme/workflows:review"], false),
            (vec!["review", "--target-from", "."], false),
        ] {
            let args = super::super::test_support::parse_run_args(flags).unwrap();
            assert_eq!(Interruption::for_run_args(&args).listens, listens);
        }
    }

    struct Fixture {
        root: tempfile::TempDir,
        repo: git2::Repository,
        git:  NativeGit,
        sha:  String,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let repo_dir = root.path().join("source");
            let repo = git2::Repository::init_opts(
                &repo_dir,
                git2::RepositoryInitOptions::new().initial_head("trunk"),
            )
            .unwrap();
            write_workflow(&repo_dir, ".fabro/workflows/review");
            let sha = commit_all(&repo, "workflow");
            let config = root.path().join("gitconfig");
            std::fs::write(
                &config,
                format!(
                    "[url \"file://{}\"]\n    insteadOf = https://github.com/acme/workflows\n",
                    repo_dir.display()
                ),
            )
            .unwrap();
            let mut git = NativeGit::new(GitHubHost::default());
            git.environment = vec![
                ("GIT_CONFIG_GLOBAL".into(), config.to_str().unwrap().into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_COUNT".into(), "0".into()),
            ];
            Self {
                root,
                repo,
                git,
                sha,
            }
        }

        fn repository() -> GitHubRepositorySlug {
            "acme/workflows".parse().unwrap()
        }
    }

    #[tokio::test]
    async fn remote_workflow_cleanup_preserves_sibling_identity_and_disables_filters_hooks() {
        let fixture = Fixture::new();
        let source = fixture.repo.workdir().unwrap();
        let child = source.join(".fabro/workflows/child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(
            child.join("workflow.fabro"),
            "digraph Child { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();
        std::fs::write(source.join(".fabro/workflows/review/workflow.fabro"), "digraph Root { start [shape=Mdiamond] exit [shape=Msquare] child [shape=house, stack.child_workflow=\"../child/workflow.fabro\"] start -> child -> exit }").unwrap();
        std::fs::write(source.join(".gitattributes"), "*.fabro filter=fixture\n").unwrap();
        let config_path = fixture.root.path().join("gitconfig");
        let mut config = git2::Config::open(&config_path).unwrap();
        let sentinel = fixture.root.path().join("executed");
        let hooks = fixture.root.path().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        std::fs::write(
            hooks.join("post-checkout"),
            format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
        )
        .unwrap();
        std::fs::set_permissions(
            hooks.join("post-checkout"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        // The fsmonitor hook runs during checkout even with hooksPath disabled.
        std::fs::write(
            hooks.join("fsmonitor"),
            format!(
                "#!/bin/sh\ntouch '{}'\nprintf 'token\\0/\\0'\n",
                sentinel.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            hooks.join("fsmonitor"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        config
            .set_str("core.hooksPath", hooks.to_str().unwrap())
            .unwrap();
        config
            .set_str("core.fsmonitor", hooks.join("fsmonitor").to_str().unwrap())
            .unwrap();
        config
            .set_str(
                "filter.fixture.smudge",
                &format!("touch '{}'; cat", sentinel.display()),
            )
            .unwrap();
        let sha = commit_all(&fixture.repo, "update workflow");
        let local = fabro_manifest::collect_workflow_versions(Path::new("review"), source).unwrap();
        assert_eq!(local.versions().count(), 2);
        let cancel = CancellationToken::new();
        for selector in [
            "review",
            ".fabro/workflows/review/workflow.fabro",
            "missing",
        ] {
            let checkout = fixture.git.scratch_repository(&cancel).await.unwrap();
            let checkout_path = checkout.path().to_path_buf();
            let result = fixture
                .git
                .collect_checkout(
                    Fixture::repository(),
                    selector.into(),
                    sha.clone(),
                    checkout,
                    CancellationToken::new(),
                )
                .await;
            assert!(!checkout_path.exists());
            if selector == "missing" {
                assert!(result.is_err());
            } else {
                let remote = result.unwrap();
                assert_eq!(local.root_id(), remote.root_id());
                assert_eq!(
                    local.versions().map(|(id, _)| id).collect::<Vec<_>>(),
                    remote.versions().map(|(id, _)| id).collect::<Vec<_>>()
                );
            }
            assert!(!sentinel.exists());
        }
        let checkout = fixture.git.scratch_repository(&cancel).await.unwrap();
        let path = checkout.path().to_path_buf();
        assert!(
            fixture
                .git
                .collect_checkout(
                    Fixture::repository(),
                    "review".into(),
                    "1111111111111111111111111111111111111111".into(),
                    checkout,
                    cancel
                )
                .await
                .is_err()
        );
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn remote_workflow_tolerates_symlinks_the_selected_workflow_never_reads() {
        let fixture = Fixture::new();
        let source = fixture.repo.workdir().unwrap();
        // Submodule-style dangling links and links outside the checkout are
        // common in workflow repositories and irrelevant to the selection.
        std::os::unix::fs::symlink("../missing-submodule", source.join("vendor")).unwrap();
        std::os::unix::fs::symlink("/usr/local/lib/node_modules", source.join("tools")).unwrap();
        let sha = commit_all(&fixture.repo, "add links");
        let local = fabro_manifest::collect_workflow_versions(Path::new("review"), source).unwrap();
        let remote = fixture
            .git
            .collect(
                Fixture::repository(),
                "review".into(),
                RemoteWorkflowRevision::Commit(sha),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(local.root_id(), remote.root_id());
    }

    #[tokio::test]
    async fn remote_workflow_rejects_toml_symlink_before_reading_host_content() {
        let fixture = Fixture::new();
        let host = tempfile::tempdir().unwrap();
        let fifo = host.path().join("host.toml");
        // Any accidental read blocks: there is deliberately no writer. The
        // successful containment error proves rejection before TOML loading.
        unistd::mkfifo(&fifo, Mode::S_IRUSR).unwrap();
        let path = fixture
            .repo
            .workdir()
            .unwrap()
            .join(".fabro/workflows/review/workflow.toml");
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&fifo, path).unwrap();
        let sha = commit_all(&fixture.repo, "update workflow");
        let error = fixture
            .git
            .collect(
                Fixture::repository(),
                "review".into(),
                RemoteWorkflowRevision::Commit(sha),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            format!("{error:?}").contains("outside its source root"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn remote_workflow_dropped_waiter_cancels_child_before_checkout_cleanup() {
        let (fake, git) = fake_git("printf '%s' $$ > pid; exec /bin/sleep 60");
        let checkout = tempfile::tempdir().unwrap();
        let path = checkout.path().to_path_buf();
        let worker = tokio::spawn(async move {
            Interruption::new(true)
                .owned(move |cancel| async move {
                    git.collect_checkout(
                        "acme/workflows".parse().unwrap(),
                        "review".into(),
                        "1111111111111111111111111111111111111111".into(),
                        checkout,
                        cancel,
                    )
                    .await
                })
                .await
        });
        time::timeout(Duration::from_secs(5), async {
            while !path.join("pid").exists() {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pid: u32 = std::fs::read_to_string(path.join("pid"))
            .unwrap()
            .parse()
            .unwrap();
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        time::timeout(Duration::from_secs(5), async {
            while path.exists() {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!fabro_proc::process_exists(pid));
        drop(fake);
    }

    /// Each nextest test runs in its own process, so raising SIGINT here only
    /// reaches the listener `guard` installed before polling `work`.
    #[tokio::test]
    async fn remote_workflow_guard_cancels_owned_tasks_and_waits_for_cleanup_on_ctrl_c() {
        let interruption = Interruption::new(true);
        let cleaned = Arc::new(AtomicBool::new(false));
        let result: anyhow::Result<()> = interruption
            .guard({
                let interruption = interruption.clone();
                let cleaned = Arc::clone(&cleaned);
                async move {
                    interruption
                        .owned(move |cancel| async move {
                            signal::raise(Signal::SIGINT).unwrap();
                            cancel.cancelled().await;
                            // Cleanup after cancellation must finish before
                            // `guard` reports the interruption.
                            time::sleep(Duration::from_millis(200)).await;
                            cleaned.store(true, Ordering::SeqCst);
                            Ok(())
                        })
                        .await
                }
            })
            .await;
        assert!(matches!(
            result.unwrap_err().downcast_ref::<RemoteWorkflowError>(),
            Some(RemoteWorkflowError::Cancelled)
        ));
        assert!(cleaned.load(Ordering::SeqCst));
        assert_eq!(
            Interruption::new(false)
                .guard(async { Ok::<_, anyhow::Error>(7) })
                .await
                .unwrap(),
            7
        );
    }

    #[tokio::test]
    async fn remote_workflow_resolves_exact_default_branches_tags_and_commits() {
        let fixture = Fixture::new();
        let object = fixture.repo.revparse_single("HEAD").unwrap();
        fixture
            .repo
            .branch("topic/slash", object.as_commit().unwrap(), false)
            .unwrap();
        fixture
            .repo
            .tag_lightweight("light", &object, false)
            .unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.test").unwrap();
        fixture
            .repo
            .tag("annotated", &object, &signature, "release", false)
            .unwrap();
        let cancel = CancellationToken::new();
        let scratch = fixture.git.scratch_repository(&cancel).await.unwrap();
        for reference in [
            None,
            Some("HEAD"),
            Some("trunk"),
            Some("topic/slash"),
            Some("light"),
            Some("annotated"),
            Some("refs/heads/trunk"),
            Some("refs/tags/annotated"),
            Some(fixture.sha.as_str()),
        ] {
            let revision = RemoteWorkflowRevision::parse(reference).unwrap();
            assert_eq!(
                fixture
                    .git
                    .resolve_revision(scratch.path(), &Fixture::repository(), &revision, &cancel)
                    .await
                    .unwrap(),
                fixture.sha
            );
        }
        fixture
            .repo
            .tag_lightweight("trunk", &object, false)
            .unwrap();
        assert!(
            fixture
                .git
                .resolve_revision(
                    scratch.path(),
                    &Fixture::repository(),
                    &RemoteWorkflowRevision::Name("trunk".into()),
                    &cancel
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        for reference in ["missing", "refs/tags/missing", "refs/heads/missing"] {
            assert!(
                fixture
                    .git
                    .resolve_revision(
                        scratch.path(),
                        &Fixture::repository(),
                        &RemoteWorkflowRevision::parse(Some(reference)).unwrap(),
                        &cancel
                    )
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("was not found"),
                "{reference}"
            );
        }
        let target = fixture
            .git
            .resolve_target(Fixture::repository(), None, &cancel)
            .await
            .unwrap();
        assert_eq!(target.branch, "trunk");
        assert_eq!(target.sha.as_deref(), Some(fixture.sha.as_str()));
        assert_eq!(
            fixture
                .git
                .resolve_target(Fixture::repository(), Some("topic/slash".into()), &cancel)
                .await
                .unwrap()
                .branch,
            "topic/slash"
        );
        for branch in [
            "missing",
            "annotated",
            "HEAD",
            fixture.sha.as_str(),
            "refs/heads/trunk",
        ] {
            assert!(
                fixture
                    .git
                    .resolve_target(Fixture::repository(), Some(branch.into()), &cancel)
                    .await
                    .is_err()
            );
        }
        // Metadata-only target resolution never creates a checkout directory.
        assert_eq!(std::fs::read_dir(fixture.root.path()).unwrap().count(), 2);
    }

    #[tokio::test]
    async fn remote_workflow_same_bytes_have_same_ids_and_no_lookup_fallback() {
        let fixture = Fixture::new();
        let local = fabro_manifest::collect_workflow_versions(
            Path::new("review"),
            fixture.repo.workdir().unwrap(),
        )
        .unwrap();
        let remote = fixture
            .git
            .collect(
                Fixture::repository(),
                "review".into(),
                RemoteWorkflowRevision::DefaultBranch,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(local.root_id(), remote.root_id());
        let before: Vec<_> = local.versions().map(|(id, _)| id).collect();
        assert_eq!(
            before,
            remote.versions().map(|(id, _)| id).collect::<Vec<_>>()
        );
        assert!(
            fixture
                .git
                .collect(
                    Fixture::repository(),
                    "missing".into(),
                    RemoteWorkflowRevision::DefaultBranch,
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn remote_workflow_fetches_observed_commit_after_branch_moves() {
        let fixture = Fixture::new();
        let cancel = CancellationToken::new();
        let checkout = fixture.git.scratch_repository(&cancel).await.unwrap();
        let captured = fixture
            .git
            .resolve_revision(
                checkout.path(),
                &Fixture::repository(),
                &RemoteWorkflowRevision::DefaultBranch,
                &cancel,
            )
            .await
            .unwrap();
        let parent = fixture
            .repo
            .revparse_single("HEAD")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.test").unwrap();
        let next = fixture
            .repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "move branch",
                &parent.tree().unwrap(),
                &[&parent],
            )
            .unwrap();
        assert_ne!(next.to_string(), captured);
        fixture
            .git
            .checkout(&Fixture::repository(), &captured, checkout.path(), &cancel)
            .await
            .unwrap();
        assert_eq!(
            git2::Repository::open(checkout.path())
                .unwrap()
                .head()
                .unwrap()
                .target()
                .unwrap()
                .to_string(),
            captured
        );
        assert!(
            fixture
                .git
                .checkout(
                    &Fixture::repository(),
                    "1111111111111111111111111111111111111111",
                    checkout.path(),
                    &cancel
                )
                .await
                .is_err()
        );
    }

    #[test]
    fn remote_workflow_default_target_branch_requires_a_working_branch_name() {
        let sha = "1234567890123456789012345678901234567890";
        for (head, valid) in [
            ("trunk", true),
            ("topic/slash", true),
            ("heads/main", false),
            ("tags/release", false),
            (sha, false),
        ] {
            let records = format!("ref: refs/heads/{head}\tHEAD\n{sha}\tHEAD\n");
            assert_eq!(default_head(&records).unwrap().0, head);
            let target = default_target_branch(&records);
            assert_eq!(target.is_ok(), valid, "{head}");
            if !valid {
                assert!(target.unwrap_err().to_string().contains("--target-branch"));
            }
        }
    }

    #[test]
    fn remote_workflow_matches_records_exactly() {
        let sha = "1234567890123456789012345678901234567890";
        assert!(
            resolve_name(
                &format!("{sha}\trefs/heads/nested/main\n"),
                "refs/heads/main",
                "refs/tags/main"
            )
            .is_err()
        );
    }

    fn fake_git(script: &str) -> (tempfile::TempDir, NativeGit) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("git");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut git = NativeGit::new(GitHubHost::default());
        git.environment
            .push(("PATH".into(), root.path().to_str().unwrap().into()));
        (root, git)
    }

    #[tokio::test]
    async fn remote_workflow_caps_drains_diagnostics_and_preserves_safe_status() {
        let (root, git) = fake_git(
            "i=0; while [ $i -lt 9000 ]; do printf 'sentinel-secret-plain-text\\n'; printf 'sentinel-secret-plain-text\\n' >&2; i=$((i+1)); done; exit 42",
        );
        let error = git
            .command("fetch", root.path(), &["fetch"], &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(error, RemoteWorkflowError::Process { status, .. } if status.code() == Some(42))
        );
        assert!(!format!("{error:?} {error}").contains("sentinel-secret"));
        let (root, git) = fake_git(
            "i=0; while [ $i -lt 9000 ]; do printf 'metadata-output\\n'; i=$((i+1)); done",
        );
        assert!(matches!(
            git.command(
                "metadata lookup",
                root.path(),
                &["ls-remote"],
                &CancellationToken::new()
            )
            .await
            .unwrap_err(),
            RemoteWorkflowError::OutputLimit
        ));
    }

    #[tokio::test]
    async fn remote_workflow_cancelled_command_never_spawns() {
        let (root, git) = fake_git("printf started > spawned");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = git
            .command("fetch", root.path(), &["fetch"], &cancel)
            .await
            .unwrap_err();
        assert!(matches!(error, RemoteWorkflowError::Cancelled));
        assert!(!root.path().join("spawned").exists());
    }

    #[tokio::test]
    async fn remote_workflow_spawn_failure_preserves_io_source() {
        use std::error::Error as _;

        let (root, git) = fake_git("exit 0");
        let error = git
            .command(
                "fetch",
                &root.path().join("missing"),
                &["fetch"],
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn remote_workflow_timeout_and_cancel_reap_owned_children() {
        for timeout in [true, false] {
            let (root, mut git) = fake_git("printf '%s' $$ > pid; exec /bin/sleep 60");
            git.timeout = Duration::from_secs(2);
            let cancel = CancellationToken::new();
            let trigger = async {
                if !timeout {
                    time::timeout(Duration::from_secs(5), async {
                        while !root.path().join("pid").exists() {
                            time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    cancel.cancel();
                }
            };
            let (result, ()) = tokio::join!(
                git.command("fetch", root.path(), &["fetch"], &cancel),
                trigger
            );
            let error = result.unwrap_err();
            assert!(matches!(
                error,
                RemoteWorkflowError::Timeout | RemoteWorkflowError::Cancelled
            ));
            let pid: u32 = std::fs::read_to_string(root.path().join("pid"))
                .unwrap()
                .parse()
                .unwrap();
            assert!(!fabro_proc::process_exists(pid));
        }
    }
}
