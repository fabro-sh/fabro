use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_proc::ProcessError;
use fabro_store::KeyedMutex;
use fabro_types::{GitHubRepositorySlug, GitRunTarget};
use tokio::fs;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const GIT_CLONE_TIMEOUT: Duration = Duration::from_mins(2);
const GIT_FETCH_TIMEOUT: Duration = Duration::from_mins(1);
const GIT_WORKTREE_ADD_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_WORKTREE_PRUNE_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_REV_PARSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Error returned while preparing a checkout from a git source.
#[derive(thiserror::Error, Debug)]
pub(crate) enum GitCheckoutError {
    #[error("failed to initialize the run's source repository")]
    Initialize(#[source] GitCommandError),
    #[error("failed to check out the run's selected revision")]
    Checkout(#[source] GitCommandError),
    #[error("failed to create Git cache directory {path}")]
    CacheDirectory {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to clone repository")]
    Clone {
        #[source]
        source: GitCommandError,
    },
    #[error("failed to fetch branch {branch:?}")]
    FetchBranch {
        branch: String,
        #[source]
        source: GitCommandError,
    },
    #[error("failed to fetch tag {tag:?}")]
    FetchTag {
        tag:    String,
        #[source]
        source: GitCommandError,
    },
    #[error("failed to fetch exact commit {sha}")]
    FetchCommit {
        sha:    String,
        #[source]
        source: GitCommandError,
    },
    #[error("failed to resolve fetched Git target to a commit")]
    ResolveCommit {
        #[source]
        source: GitCommandError,
    },
    #[error("failed to add Git worktree")]
    AddWorktree {
        #[source]
        source: GitCommandError,
    },
}

/// Materialize an independent source repository for Petri's checkout step.
/// It lives with the run, so later cache fetches cannot move its HEAD and a
/// delayed start needs neither credentials nor a still-existing remote ref.
/// Authentication is process-local and never written into the repository.
pub(crate) async fn prepare_run_repository(
    target: &GitRunTarget,
    clone_url: &str,
    auth: Option<&GitAuthConfig>,
    depth: i32,
    directory: &Path,
) -> Result<String, GitCheckoutError> {
    for args in [vec!["init", "--quiet"], vec![
        "remote", "add", "origin", clone_url,
    ]] {
        run_git_plan(GitCommandPlan::new(args, GIT_REV_PARSE_TIMEOUT).current_dir(directory))
            .await
            .map_err(GitCheckoutError::Initialize)?;
    }
    let selector = GitCheckoutSelector::from(target);
    let mut args = vec!["fetch".to_string(), "--no-tags".to_string()];
    if depth > 0 {
        args.push(format!("--depth={depth}"));
    }
    // Qualify branches, since a branch and a tag may have the same name.
    let revision = match selector {
        GitCheckoutSelector::Branch(branch) => format!("refs/heads/{branch}"),
        _ => selector.selector().into_owned(),
    };
    args.extend(["origin".to_string(), "--".to_string(), revision]);
    run_git_plan(
        GitCommandPlan::new(args, GIT_FETCH_TIMEOUT)
            .current_dir(directory)
            .with_auth(clone_url, auth),
    )
    .await
    .map_err(|source| selector.checkout_error(source))?;
    let sha = run_git_plan(build_rev_parse_fetch_head_plan(directory))
        .await
        .map_err(|source| GitCheckoutError::ResolveCommit { source })?;
    let sha = String::from_utf8_lossy(&sha).trim().to_owned();
    run_git_plan(
        GitCommandPlan::new(
            ["checkout", "--quiet", "-B", &target.branch, &sha],
            GIT_WORKTREE_ADD_TIMEOUT,
        )
        .current_dir(directory),
    )
    .await
    .map_err(GitCheckoutError::Checkout)?;
    Ok(sha)
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum GitCommandError {
    #[error("{command} timed out after {timeout_secs}s")]
    Timeout {
        command:      String,
        timeout_secs: u64,
    },
    #[error("failed to run {command}")]
    Spawn {
        command: String,
        #[source]
        source:  std::io::Error,
    },
    #[error("{message}")]
    Exit { message: String },
}

/// Persistent on-disk cache of bare GitHub clones, one per `(owner, repo)`.
///
/// Materializing an automation run only needs to read the workflow + its
/// supporting files out of the repo at a given ref. Cloning fresh on every
/// click costs 5-15s on the request thread. With this cache, the first
/// materialize for a repo pays the clone, and every subsequent one pays only
/// the delta `git fetch` plus a cheap `git worktree add` into the per-call
/// scratch dir.
#[derive(Debug)]
pub(crate) struct GitRepoCache {
    cache_root: PathBuf,
    locks:      KeyedMutex<(String, String)>,
}

impl GitRepoCache {
    pub(crate) fn new(cache_root: PathBuf) -> Self {
        Self {
            cache_root,
            locks: KeyedMutex::new(),
        }
    }

    fn bare_dir(&self, repo: &GitHubRepositorySlug) -> PathBuf {
        self.cache_root
            .join(repo.owner())
            .join(format!("{}.git", repo.repo()))
    }

    /// Prepare a worktree containing the requested ref of `repo`, fetched from
    /// `clone_url`, at
    /// `worktree_dir`. Returns the resolved commit SHA.
    ///
    /// First call for a repo: a `--bare --depth 1` clone is created at
    /// `<cache>/<owner>/<repo>.git`. Subsequent calls reuse the bare clone
    /// and only `git fetch --depth 1` the requested ref. In both cases a
    /// short-lived worktree is added at `worktree_dir`; the caller owns its
    /// lifetime (typically a `TempDir`). If stale worktree admin entries from
    /// crashed prior calls block the add, the cache prunes those entries and
    /// retries once.
    pub(crate) async fn prepare_worktree(
        &self,
        args: WorktreePrepareInput<'_>,
        clone_url: &str,
    ) -> Result<String, GitCheckoutError> {
        let _guard = self
            .locks
            .lock((args.repo.owner().to_string(), args.repo.repo().to_string()))
            .await;
        let bare_dir = self.bare_dir(args.repo);

        match self.try_prepare_worktree(&bare_dir, &args, clone_url).await {
            Ok(sha) => Ok(sha),
            Err(first_err) if bare_clone_may_be_corrupt(&bare_dir).await => {
                // Best-effort wipe and one retry. Corruption is rare; auth
                // and network failures don't trip this branch because
                // `bare_clone_may_be_corrupt` only returns true when the
                // bare repo's `HEAD` file is missing or empty.
                if let Err(remove_err) = fs::remove_dir_all(&bare_dir).await {
                    tracing::warn!(
                        path = %bare_dir.display(),
                        error = %remove_err,
                        "failed to remove corrupt cached git repository"
                    );
                    return Err(first_err);
                }
                self.try_prepare_worktree(&bare_dir, &args, clone_url)
                    .await
                    .map_err(|_| first_err)
            }
            Err(err) => Err(err),
        }
    }

    async fn try_prepare_worktree(
        &self,
        bare_dir: &Path,
        args: &WorktreePrepareInput<'_>,
        clone_url: &str,
    ) -> Result<String, GitCheckoutError> {
        let bare_exists = fs::try_exists(&bare_dir.join("HEAD"))
            .await
            .unwrap_or(false);
        if !bare_exists {
            if let Some(parent) = bare_dir.parent() {
                fs::create_dir_all(parent).await.map_err(|err| {
                    GitCheckoutError::CacheDirectory {
                        path:   parent.to_path_buf(),
                        source: err,
                    }
                })?;
            }
            run_git_plan(build_bare_clone_plan(clone_url, bare_dir, args.auth))
                .await
                .map_err(|source| GitCheckoutError::Clone { source })?;
        }

        let fetch_target = args.selector;
        run_git_plan(build_bare_fetch_plan(
            bare_dir,
            clone_url,
            &fetch_target.selector(),
            args.auth,
        ))
        .await
        .map_err(|source| fetch_target.checkout_error(source))?;

        let checked_out_sha = run_git_plan(build_rev_parse_fetch_head_plan(bare_dir))
            .await
            .map_err(|source| GitCheckoutError::ResolveCommit { source })
            .map(|stdout| String::from_utf8_lossy(&stdout).trim().to_string())?;

        add_worktree_with_stale_retry(bare_dir, args.worktree_dir, &checked_out_sha).await?;

        Ok(checked_out_sha)
    }
}

pub(crate) struct WorktreePrepareInput<'a> {
    pub repo:         &'a GitHubRepositorySlug,
    pub selector:     GitCheckoutSelector<'a>,
    pub auth:         Option<&'a GitAuthConfig>,
    pub worktree_dir: &'a Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitCheckoutSelector<'a> {
    Branch(&'a str),
    Tag(&'a str),
    Commit(&'a str),
}

impl<'a> From<&'a GitRunTarget> for GitCheckoutSelector<'a> {
    fn from(target: &'a GitRunTarget) -> Self {
        if let Some(sha) = target.sha.as_deref() {
            Self::Commit(sha)
        } else if let Some(tag) = target.tag.as_deref() {
            Self::Tag(tag)
        } else {
            Self::Branch(&target.branch)
        }
    }
}

impl GitCheckoutSelector<'_> {
    fn selector(&self) -> Cow<'_, str> {
        match self {
            Self::Branch(selector) | Self::Commit(selector) => Cow::Borrowed(selector),
            Self::Tag(tag) => Cow::Owned(format!("refs/tags/{tag}")),
        }
    }

    fn checkout_error(&self, source: GitCommandError) -> GitCheckoutError {
        match self {
            Self::Branch(branch) => GitCheckoutError::FetchBranch {
                branch: (*branch).to_string(),
                source,
            },
            Self::Tag(tag) => GitCheckoutError::FetchTag {
                tag: (*tag).to_string(),
                source,
            },
            Self::Commit(sha) => GitCheckoutError::FetchCommit {
                sha: (*sha).to_string(),
                source,
            },
        }
    }
}

async fn bare_clone_may_be_corrupt(bare_dir: &Path) -> bool {
    match fs::metadata(&bare_dir.join("HEAD")).await {
        Ok(meta) => meta.len() == 0,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::try_exists(bare_dir).await.unwrap_or(false)
        }
        Err(err) => {
            tracing::warn!(
                path = %bare_dir.join("HEAD").display(),
                %err,
                "Failed to inspect cached git repository HEAD"
            );
            false
        }
    }
}

pub(crate) fn github_clone_url(repo: &GitHubRepositorySlug) -> String {
    let mut url = repo.https_url();
    url.push_str(".git");
    url
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitAuthConfig {
    extraheader:      String,
    sensitive_values: Vec<String>,
}

impl GitAuthConfig {
    pub(crate) fn new(credentials: &fabro_github::GitCloneCredentials) -> Self {
        Self::from_parts(credentials.username(), credentials.password())
    }

    pub(crate) fn from_parts(username: &str, password: &str) -> Self {
        let encoded_credentials = BASE64_STANDARD.encode(format!("{username}:{password}"));
        let extraheader = basic_auth_header_from_encoded(&encoded_credentials);
        Self {
            sensitive_values: vec![
                password.to_string(),
                encoded_credentials,
                extraheader.clone(),
            ],
            extraheader,
        }
    }

    fn git_env(&self, clone_url: &str) -> Vec<(String, String)> {
        vec![
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            (
                "GIT_CONFIG_KEY_0".to_string(),
                format!("http.{clone_url}.extraheader"),
            ),
            ("GIT_CONFIG_VALUE_0".to_string(), self.extraheader.clone()),
        ]
    }

    fn sensitive_values(&self) -> &[String] {
        &self.sensitive_values
    }
}

pub(crate) async fn resolve_git_read_auth_config(
    credentials: Option<&fabro_github::GitHubCredentials>,
    repo: &GitHubRepositorySlug,
    github_api_base_url: &str,
    http_client: Option<fabro_http::HttpClient>,
) -> anyhow::Result<Option<GitAuthConfig>> {
    let Some(credentials) = credentials else {
        return Ok(None);
    };
    let context = match http_client {
        Some(client) => {
            fabro_github::GitHubContext::with_http_client(credentials, github_api_base_url, client)
        }
        None => fabro_github::GitHubContext::new(credentials, github_api_base_url),
    };
    let credentials =
        fabro_github::resolve_read_only_clone_credentials(&context, repo.owner(), repo.repo())
            .await?;
    Ok(Some(GitAuthConfig::new(&credentials)))
}

#[cfg(test)]
fn basic_auth_header(username: &str, password: &str) -> String {
    basic_auth_header_from_encoded(&BASE64_STANDARD.encode(format!("{username}:{password}")))
}

fn basic_auth_header_from_encoded(encoded_credentials: &str) -> String {
    format!("AUTHORIZATION: basic {encoded_credentials}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitCommandPlan {
    program:          String,
    args:             Vec<String>,
    env:              Vec<(String, String)>,
    current_dir:      Option<PathBuf>,
    timeout:          Duration,
    sensitive_values: Vec<String>,
}

impl GitCommandPlan {
    fn new(args: impl IntoIterator<Item = impl Into<String>>, timeout: Duration) -> Self {
        Self {
            program: "git".to_string(),
            args: args.into_iter().map(Into::into).collect(),
            env: vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())],
            current_dir: None,
            timeout,
            sensitive_values: Vec::new(),
        }
    }

    fn current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(current_dir.into());
        self
    }

    fn with_auth(mut self, clone_url: &str, auth: Option<&GitAuthConfig>) -> Self {
        if let Some(auth) = auth {
            self.env.extend(auth.git_env(clone_url));
            self.sensitive_values
                .extend(auth.sensitive_values().iter().cloned());
        }
        self
    }

    #[cfg(test)]
    fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn build_bare_clone_plan(
    clone_url: &str,
    bare_dir: &Path,
    auth: Option<&GitAuthConfig>,
) -> GitCommandPlan {
    GitCommandPlan::new(
        [
            "clone".to_string(),
            "--bare".to_string(),
            "--depth".to_string(),
            "1".to_string(),
            clone_url.to_string(),
            bare_dir.display().to_string(),
        ],
        GIT_CLONE_TIMEOUT,
    )
    .with_auth(clone_url, auth)
}

fn build_bare_fetch_plan(
    bare_dir: &Path,
    clone_url: &str,
    ref_selector: &str,
    auth: Option<&GitAuthConfig>,
) -> GitCommandPlan {
    GitCommandPlan::new(
        [
            "fetch".to_string(),
            "--depth".to_string(),
            "1".to_string(),
            "origin".to_string(),
            "--".to_string(),
            ref_selector.to_string(),
        ],
        GIT_FETCH_TIMEOUT,
    )
    .current_dir(bare_dir)
    .with_auth(clone_url, auth)
}

fn build_worktree_add_plan(bare_dir: &Path, worktree_dir: &Path, target: &str) -> GitCommandPlan {
    GitCommandPlan::new(
        [
            "worktree".to_string(),
            "add".to_string(),
            "--detach".to_string(),
            "--force".to_string(),
            worktree_dir.display().to_string(),
            target.to_string(),
        ],
        GIT_WORKTREE_ADD_TIMEOUT,
    )
    .current_dir(bare_dir)
}

fn build_worktree_prune_plan(bare_dir: &Path) -> GitCommandPlan {
    GitCommandPlan::new(["worktree", "prune"], GIT_WORKTREE_PRUNE_TIMEOUT).current_dir(bare_dir)
}

fn build_rev_parse_fetch_head_plan(bare_dir: &Path) -> GitCommandPlan {
    GitCommandPlan::new(["rev-parse", "FETCH_HEAD^{commit}"], GIT_REV_PARSE_TIMEOUT)
        .current_dir(bare_dir)
}

async fn add_worktree_with_stale_retry(
    bare_dir: &Path,
    worktree_dir: &Path,
    target: &str,
) -> Result<(), GitCheckoutError> {
    match run_git_plan(build_worktree_add_plan(bare_dir, worktree_dir, target)).await {
        Ok(_) => Ok(()),
        Err(first_err) => {
            tracing::warn!(
                error = ?first_err,
                bare_dir = %bare_dir.display(),
                worktree_dir = %worktree_dir.display(),
                "git worktree add failed; pruning stale worktree entries and retrying"
            );
            if let Err(prune_err) = run_git_plan(build_worktree_prune_plan(bare_dir)).await {
                tracing::warn!(
                    error = ?prune_err,
                    bare_dir = %bare_dir.display(),
                    "failed to prune stale git worktree entries"
                );
            }
            run_git_plan(build_worktree_add_plan(bare_dir, worktree_dir, target))
                .await
                .map(|_| ())
                .map_err(|source| GitCheckoutError::AddWorktree { source })
        }
    }
}

async fn run_git_plan(plan: GitCommandPlan) -> Result<Vec<u8>, GitCommandError> {
    let mut command = Command::new(&plan.program);
    command.args(&plan.args);
    command.envs(plan.env.iter().map(|(key, value)| (key, value)));
    if let Some(current_dir) = plan.current_dir.as_ref() {
        command.current_dir(current_dir);
    }
    let output = fabro_proc::capture(
        &mut command,
        Some(plan.timeout),
        &CancellationToken::new(),
        None,
    )
    .await
    .map_err(|err| match err {
        ProcessError::TimedOut => GitCommandError::Timeout {
            command:      safe_command_label(&plan),
            timeout_secs: plan.timeout.as_secs(),
        },
        ProcessError::Io(source) => GitCommandError::Spawn {
            command: safe_command_label(&plan),
            source,
        },
        ProcessError::Cancelled => GitCommandError::Spawn {
            command: safe_command_label(&plan),
            source:  std::io::Error::new(std::io::ErrorKind::Interrupted, err),
        },
    })?
    .output;

    if output.status.success() {
        return Ok(output.stdout);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut message = format!(
        "{} exited with status {}",
        safe_command_label(&plan),
        output.status
    );
    if !stderr.trim().is_empty() {
        message.push_str(": ");
        message.push_str(stderr.trim());
    } else if !stdout.trim().is_empty() {
        message.push_str(": ");
        message.push_str(stdout.trim());
    }
    Err(GitCommandError::Exit {
        message: redact_git_output(&message, &plan.sensitive_values),
    })
}

fn safe_command_label(plan: &GitCommandPlan) -> String {
    if plan.args.is_empty() {
        plan.program.clone()
    } else {
        format!("{} {}", plan.program, plan.args.join(" "))
    }
}

fn redact_git_output(text: &str, sensitive_values: &[String]) -> String {
    let mut redacted = fabro_redact::redact_string(text);
    for value in sensitive_values
        .iter()
        .map(String::as_str)
        .filter(|value| !value.is_empty())
    {
        redacted = redacted.replace(value, "REDACTED");
    }
    redacted
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "Git checkout unit tests build local git repositories and inspect temp files synchronously."
    )]

    use std::error::Error;
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;
    use tokio::{task, time};

    use super::*;

    fn shell_plan(script: &str, directory: &Path) -> GitCommandPlan {
        GitCommandPlan {
            program:          "sh".to_string(),
            args:             vec!["-c".to_string(), script.to_string()],
            env:              vec![
                ("HOME".to_string(), directory.display().to_string()),
                ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
                ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
            ],
            current_dir:      Some(directory.to_path_buf()),
            timeout:          Duration::from_secs(5),
            sensitive_values: vec![],
        }
    }

    #[tokio::test]
    async fn git_plan_capture_preserves_bytes_environment_and_diagnostics() {
        let temp = TempDir::new().unwrap();
        let mut plan = shell_plan(
            "printf '%s' \"$TEST_CAPTURE_VALUE\"; head -c 200000 /dev/zero; printf '\\377'",
            temp.path(),
        );
        plan.env.push((
            "TEST_CAPTURE_VALUE".to_string(),
            "literal $(command)".to_string(),
        ));
        let output = run_git_plan(plan).await.unwrap();
        assert!(output.starts_with(b"literal $(command)"));
        assert_eq!(output.len(), 200_019);
        assert_eq!(output.last(), Some(&255));

        let mut plan = shell_plan(
            "printf stdout; printf '%s' \"$TEST_CAPTURE_VALUE\" >&2; exit 7",
            temp.path(),
        );
        let secret = "fixture-secret-value";
        plan.env
            .push(("TEST_CAPTURE_VALUE".to_string(), secret.to_string()));
        plan.sensitive_values.push(secret.to_string());
        let error = run_git_plan(plan).await.unwrap_err();
        let message = error.to_string();
        assert!(!message.contains(secret));
        assert!(message.ends_with("REDACTED"));

        let mut plan = shell_plan("exit 0", temp.path());
        plan.program = "/nonexistent/fabro-test-git".to_string();
        let error = run_git_plan(plan).await.unwrap_err();
        assert!(Error::source(&error).unwrap().is::<std::io::Error>());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_plan_deadline_includes_held_pipe() {
        struct Cleanup(TempDir);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(pid) = fs::read_to_string(self.0.path().join("leader.pid"))
                    .ok()
                    .and_then(|value| value.trim().parse().ok())
                {
                    fabro_proc::sigkill_process_group(pid);
                }
            }
        }
        let fixture = Cleanup(TempDir::new().unwrap());
        let mut plan = shell_plan(
            "echo $$ > leader.pid; sleep 60 & echo $! > helper.pid; exit 0",
            fixture.0.path(),
        );
        plan.timeout = Duration::from_millis(200);
        let error = time::timeout(Duration::from_secs(8), run_git_plan(plan))
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, GitCommandError::Timeout { .. }));
        let helper: u32 = fs::read_to_string(fixture.0.path().join("helper.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        time::timeout(Duration::from_secs(5), async {
            while task::spawn_blocking(move || fabro_proc::process_running_strict(helper))
                .await
                .unwrap()
            {
                time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    fn repository_slug(value: &str) -> GitHubRepositorySlug {
        GitHubRepositorySlug::try_new(value).expect("slug should parse")
    }

    fn git_target(branch: &str, tag: Option<&str>, sha: Option<&str>) -> GitRunTarget {
        GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: branch.to_string(),
            tag:    tag.map(str::to_string),
            sha:    sha.map(str::to_string),
        }
    }

    fn workflow_source(branch: &str, tag: Option<&str>, sha: Option<&str>) -> GitRunTarget {
        GitRunTarget {
            repo:   "fabro-sh/workflows".to_string(),
            branch: branch.to_string(),
            tag:    tag.map(str::to_string),
            sha:    sha.map(str::to_string),
        }
    }

    #[test]
    fn checkout_selectors_use_sha_then_tag_then_branch_precedence() {
        let target = git_target(
            "main",
            Some("v1"),
            Some("abcdef0123456789abcdef0123456789abcdef01"),
        );
        assert_eq!(
            GitCheckoutSelector::from(&target),
            GitCheckoutSelector::Commit("abcdef0123456789abcdef0123456789abcdef01")
        );

        for (source, expected) in [
            (
                workflow_source("main", None, None),
                GitCheckoutSelector::Branch("main"),
            ),
            (
                workflow_source("main", Some("v1"), None),
                GitCheckoutSelector::Tag("v1"),
            ),
            (
                workflow_source(
                    "unrelated-context",
                    Some("v1"),
                    Some("abcdef0123456789abcdef0123456789abcdef01"),
                ),
                GitCheckoutSelector::Commit("abcdef0123456789abcdef0123456789abcdef01"),
            ),
        ] {
            assert_eq!(GitCheckoutSelector::from(&source), expected);
        }
    }

    #[test]
    fn checkout_reuse_identity_folds_only_repository_case() {
        assert_eq!(
            repository_slug("Fabro-Sh/Workflows"),
            repository_slug("fabro-sh/workflows")
        );
        assert_ne!(
            GitCheckoutSelector::Branch("Main"),
            GitCheckoutSelector::Branch("main")
        );
        assert_ne!(
            GitCheckoutSelector::Branch("v1"),
            GitCheckoutSelector::Tag("v1")
        );
        assert_eq!(
            GitCheckoutSelector::Commit("abcdef0123456789abcdef0123456789abcdef01"),
            GitCheckoutSelector::Commit("abcdef0123456789abcdef0123456789abcdef01")
        );
    }

    #[test]
    fn target_repository_urls_are_github_metadata_urls_without_credentials() {
        let repo = repository_slug("fabro-sh/fabro");

        assert_eq!(repo.owner(), "fabro-sh");
        assert_eq!(repo.repo(), "fabro");
        assert_eq!(
            github_clone_url(&repo),
            "https://github.com/fabro-sh/fabro.git"
        );
        assert_eq!(repo.https_url(), "https://github.com/fabro-sh/fabro");
        assert!(!github_clone_url(&repo).contains('@'));
    }

    #[test]
    fn target_repository_validation_matches_automation_validation() {
        let repo = repository_slug("owner/.github");

        assert_eq!(repo.owner(), "owner");
        assert_eq!(repo.repo(), ".github");
        assert_eq!(repo.https_url(), "https://github.com/owner/.github");
    }

    #[test]
    fn bare_cache_command_plans_use_argv_prompt_disable_and_timeouts() {
        let repo = repository_slug("fabro-sh/fabro");
        let clone_url = github_clone_url(&repo);
        let temp = TempDir::new().unwrap();
        let bare_dir = temp.path().join("fabro-sh/fabro.git");
        let worktree_dir = temp.path().join("worktree/repo");

        let clone = build_bare_clone_plan(&clone_url, &bare_dir, None);
        assert_eq!(clone.program, "git");
        assert_eq!(clone.args, vec![
            "clone",
            "--bare",
            "--depth",
            "1",
            "https://github.com/fabro-sh/fabro.git",
            bare_dir.to_str().unwrap(),
        ]);
        assert_eq!(clone.timeout, Duration::from_mins(2));
        assert_eq!(clone.env_value("GIT_TERMINAL_PROMPT"), Some("0"));

        let fetch = build_bare_fetch_plan(&bare_dir, &clone_url, "feature/materialize", None);
        assert_eq!(fetch.args, vec![
            "fetch",
            "--depth",
            "1",
            "origin",
            "--",
            "feature/materialize",
        ]);
        assert_eq!(fetch.current_dir.as_deref(), Some(bare_dir.as_path()));
        assert_eq!(fetch.timeout, Duration::from_mins(1));
        assert_eq!(fetch.env_value("GIT_TERMINAL_PROMPT"), Some("0"));

        let worktree = build_worktree_add_plan(
            &bare_dir,
            &worktree_dir,
            "0123456789abcdef0123456789abcdef01234567",
        );
        assert_eq!(worktree.args, vec![
            "worktree",
            "add",
            "--detach",
            "--force",
            worktree_dir.to_str().unwrap(),
            "0123456789abcdef0123456789abcdef01234567",
        ]);
        assert_eq!(worktree.current_dir.as_deref(), Some(bare_dir.as_path()));
        assert_eq!(worktree.timeout, Duration::from_secs(30));
        assert_eq!(worktree.env_value("GIT_TERMINAL_PROMPT"), Some("0"));

        let prune = build_worktree_prune_plan(&bare_dir);
        assert_eq!(prune.args, vec!["worktree", "prune"]);
        assert_eq!(prune.current_dir.as_deref(), Some(bare_dir.as_path()));
        assert_eq!(prune.timeout, Duration::from_secs(10));

        let rev_parse = build_rev_parse_fetch_head_plan(&bare_dir);
        assert_eq!(rev_parse.args, vec!["rev-parse", "FETCH_HEAD^{commit}"]);
        assert_eq!(rev_parse.current_dir.as_deref(), Some(bare_dir.as_path()));
        assert_eq!(rev_parse.timeout, Duration::from_secs(10));
    }

    #[test]
    fn credential_redaction_removes_tokens_and_basic_auth_headers() {
        let secret = "ghu_materializer_secret";
        let basic = basic_auth_header("x-access-token", secret);
        let message = format!(
            "fatal: could not read Username for https://github.com/fabro-sh/fabro.git; token={secret}; header={basic}"
        );

        let redacted = redact_git_output(&message, &[secret.to_string(), basic.clone()]);

        assert!(!redacted.contains(secret), "token leaked: {redacted}");
        assert!(
            !redacted.contains(&basic),
            "basic header leaked: {redacted}"
        );
        let encoded_secret = BASE64_STANDARD.encode(format!("x-access-token:{secret}"));
        assert!(
            !redacted.contains(&encoded_secret),
            "encoded credential leaked: {redacted}"
        );
        assert!(
            redacted.contains("REDACTED"),
            "expected redaction marker: {redacted}"
        );
    }

    #[test]
    fn credential_config_env_keeps_clone_url_uncredentialed() {
        let repo = repository_slug("fabro-sh/fabro");
        let clone_url = github_clone_url(&repo);
        let auth = GitAuthConfig::from_parts("x-access-token", "ghu_secret");
        let plan = build_bare_clone_plan(&clone_url, Path::new("/tmp/fabro-checkout"), Some(&auth));

        assert!(
            plan.args
                .iter()
                .any(|arg| arg == "https://github.com/fabro-sh/fabro.git")
        );
        assert!(plan.args.iter().all(|arg| !arg.contains("ghu_secret")));
        assert_eq!(plan.env_value("GIT_CONFIG_COUNT"), Some("1"));
        assert_eq!(
            plan.env_value("GIT_CONFIG_KEY_0"),
            Some("http.https://github.com/fabro-sh/fabro.git.extraheader")
        );
        assert!(
            plan.env_value("GIT_CONFIG_VALUE_0")
                .is_some_and(|value| value.starts_with("AUTHORIZATION: basic "))
        );
    }

    fn seed_upstream(upstream: &Path) -> String {
        std::process::Command::new("git")
            .args([
                "init",
                "--bare",
                "--initial-branch=main",
                upstream.to_str().unwrap(),
            ])
            .status()
            .expect("git init --bare");
        let work = upstream.parent().unwrap().join("seed");
        fs::create_dir_all(&work).unwrap();
        std::process::Command::new("git")
            .args(["init", "--initial-branch=main", work.to_str().unwrap()])
            .status()
            .expect("git init seed");
        let configure = |key: &str, value: &str| {
            std::process::Command::new("git")
                .args(["-C", work.to_str().unwrap(), "config", key, value])
                .status()
                .expect("git config seed");
        };
        configure("user.email", "test@fabro.sh");
        configure("user.name", "Fabro Test");
        configure("commit.gpgsign", "false");
        fs::write(work.join("README.md"), "seed\n").unwrap();
        std::process::Command::new("git")
            .args(["-C", work.to_str().unwrap(), "add", "."])
            .status()
            .expect("git add seed");
        std::process::Command::new("git")
            .args(["-C", work.to_str().unwrap(), "commit", "-m", "seed"])
            .status()
            .expect("git commit seed");
        std::process::Command::new("git")
            .args(["-C", work.to_str().unwrap(), "tag", "-a", "v1", "-m", "v1"])
            .status()
            .expect("git tag seed");
        std::process::Command::new("git")
            .args([
                "-C",
                work.to_str().unwrap(),
                "push",
                "--follow-tags",
                upstream.to_str().unwrap(),
                "main",
            ])
            .status()
            .expect("git push seed");
        let output = std::process::Command::new("git")
            .args(["-C", work.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse seed");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn objects_signature(dir: &Path) -> Vec<(String, u64)> {
        let mut files = Vec::new();
        let mut stack = vec![dir.join("objects")];
        while let Some(path) = stack.pop() {
            let Ok(iter) = fs::read_dir(&path) else {
                continue;
            };
            for entry in iter.flatten() {
                let entry_path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(entry_path);
                } else if meta.is_file() {
                    let rel = entry_path
                        .strip_prefix(dir.join("objects"))
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    files.push((rel, meta.len()));
                }
            }
        }
        files.sort();
        files
    }

    #[tokio::test]
    async fn run_repository_pins_the_selected_revision_and_preserves_history_depth() {
        let temp = TempDir::new().unwrap();
        let upstream = temp.path().join("upstream.git");
        let first = seed_upstream(&upstream);
        let seed = temp.path().join("seed");
        let git = |directory: &Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(directory)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        fs::write(seed.join("README.md"), "moved\n").unwrap();
        git(&seed, &["commit", "-am", "move main"]);
        let head = git(&seed, &["rev-parse", "HEAD"]);
        git(&seed, &["push", upstream.to_str().unwrap(), "main"]);
        let url = format!("file://{}", upstream.display());
        for (sha, tag, depth, expected, count) in [
            (
                Some(first.clone()),
                Some("missing".to_string()),
                1,
                &first,
                "1",
            ),
            (None, Some("v1".to_string()), 1, &first, "1"),
            (None, None, 1, &head, "1"),
            (None, None, 0, &head, "2"),
        ] {
            let directory = TempDir::new().unwrap();
            let target = GitRunTarget {
                repo: "acme/repo".to_owned(),
                branch: "main".to_owned(),
                tag,
                sha,
            };
            let actual = prepare_run_repository(&target, &url, None, depth, directory.path())
                .await
                .unwrap();
            assert_eq!(&actual, expected);
            assert_eq!(git(directory.path(), &["rev-parse", "HEAD"]), actual);
            assert_eq!(
                git(directory.path(), &["symbolic-ref", "--short", "HEAD"]),
                "main"
            );
            assert_eq!(
                git(directory.path(), &["rev-list", "--count", "HEAD"]),
                count
            );
            assert_eq!(git(directory.path(), &["remote", "get-url", "origin"]), url);
            assert!(
                !fs::read_to_string(directory.path().join(".git/config"))
                    .unwrap()
                    .contains("extraheader")
            );
        }
    }

    #[tokio::test]
    async fn run_repository_refuses_an_unavailable_revision_with_its_cause() {
        let temp = TempDir::new().unwrap();
        let upstream = temp.path().join("upstream.git");
        seed_upstream(&upstream);
        let directory = TempDir::new().unwrap();
        let target = GitRunTarget {
            repo:   "acme/repo".to_owned(),
            branch: "missing".to_owned(),
            tag:    None,
            sha:    None,
        };
        let error = prepare_run_repository(
            &target,
            &format!("file://{}", upstream.display()),
            None,
            1,
            directory.path(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, GitCheckoutError::FetchBranch { .. }));
        assert!(Error::source(&error).unwrap().is::<GitCommandError>());
        assert!(!directory.path().join("README.md").exists());
    }

    #[tokio::test]
    async fn bare_clone_reused_across_calls() {
        let temp = TempDir::new().unwrap();
        let upstream = temp.path().join("upstream.git");
        let expected_sha = seed_upstream(&upstream);
        let cache = GitRepoCache::new(temp.path().join("cache"));
        let repo = repository_slug("fabro-sh/fabro");
        let upstream_url = upstream.to_str().unwrap().to_string();
        let target = git_target("main", None, None);

        let worktree_a = temp.path().join("wt-a");
        let sha_a = cache
            .prepare_worktree(
                WorktreePrepareInput {
                    repo:         &repo,
                    selector:     GitCheckoutSelector::from(&target),
                    auth:         None,
                    worktree_dir: &worktree_a,
                },
                &upstream_url,
            )
            .await
            .expect("first prepare_worktree");
        assert_eq!(sha_a, expected_sha);
        let bare_dir = cache.bare_dir(&repo);
        assert!(bare_dir.join("HEAD").exists(), "bare clone should exist");
        let signature_before = objects_signature(&bare_dir);
        assert!(
            !signature_before.is_empty(),
            "expected object files after clone"
        );

        let worktree_b = temp.path().join("wt-b");
        let sha_b = cache
            .prepare_worktree(
                WorktreePrepareInput {
                    repo:         &repo,
                    selector:     GitCheckoutSelector::from(&target),
                    auth:         None,
                    worktree_dir: &worktree_b,
                },
                &upstream_url,
            )
            .await
            .expect("second prepare_worktree");
        assert_eq!(sha_b, expected_sha);
        let signature_after = objects_signature(&bare_dir);
        assert_eq!(
            signature_before, signature_after,
            "second call should reuse the bare clone, not re-clone",
        );
    }

    #[tokio::test]
    async fn bare_clone_recovers_from_corruption() {
        let temp = TempDir::new().unwrap();
        let upstream = temp.path().join("upstream.git");
        let expected_sha = seed_upstream(&upstream);
        let cache = GitRepoCache::new(temp.path().join("cache"));
        let repo = repository_slug("fabro-sh/fabro");
        let upstream_url = upstream.to_str().unwrap().to_string();
        let target = git_target("main", None, None);

        let worktree_a = temp.path().join("wt-a");
        cache
            .prepare_worktree(
                WorktreePrepareInput {
                    repo:         &repo,
                    selector:     GitCheckoutSelector::from(&target),
                    auth:         None,
                    worktree_dir: &worktree_a,
                },
                &upstream_url,
            )
            .await
            .expect("first prepare_worktree");

        // Simulate corruption by truncating HEAD.
        let bare_dir = cache.bare_dir(&repo);
        fs::write(bare_dir.join("HEAD"), "").unwrap();

        let worktree_b = temp.path().join("wt-b");
        let sha = cache
            .prepare_worktree(
                WorktreePrepareInput {
                    repo:         &repo,
                    selector:     GitCheckoutSelector::from(&target),
                    auth:         None,
                    worktree_dir: &worktree_b,
                },
                &upstream_url,
            )
            .await
            .expect("prepare_worktree should recover from corruption");
        assert_eq!(sha, expected_sha);
        assert!(bare_dir.join("HEAD").exists());
        assert!(
            !fs::read_to_string(bare_dir.join("HEAD"))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn tag_and_exact_commit_modes_return_the_peeled_sha() {
        let temp = TempDir::new().unwrap();
        let upstream = temp.path().join("upstream.git");
        let expected_sha = seed_upstream(&upstream);
        let cache = GitRepoCache::new(temp.path().join("cache"));
        let repo = repository_slug("fabro-sh/fabro");
        let upstream_url = upstream.to_str().unwrap().to_string();

        for (name, target) in [
            ("tag", git_target("main", Some("v1"), None)),
            (
                "pinned-tag",
                git_target("main", Some("v1"), Some(&expected_sha)),
            ),
            ("commit", git_target("main", None, Some(&expected_sha))),
        ] {
            let sha = cache
                .prepare_worktree(
                    WorktreePrepareInput {
                        repo:         &repo,
                        selector:     GitCheckoutSelector::from(&target),
                        auth:         None,
                        worktree_dir: &temp.path().join(name),
                    },
                    &upstream_url,
                )
                .await
                .expect("target should materialize");
            assert_eq!(sha, expected_sha, "{name}");
        }
    }

    #[tokio::test]
    async fn missing_tag_and_unavailable_commit_are_distinct_errors() {
        let temp = TempDir::new().unwrap();
        let upstream = temp.path().join("upstream.git");
        seed_upstream(&upstream);
        let cache = GitRepoCache::new(temp.path().join("cache"));
        let repo = repository_slug("fabro-sh/fabro");
        let upstream_url = upstream.to_str().unwrap().to_string();
        let missing_tag = git_target("main", Some("missing"), None);
        let unavailable_sha = "ffffffffffffffffffffffffffffffffffffffff";
        let unavailable_commit = git_target("main", None, Some(unavailable_sha));

        let tag_error = cache
            .prepare_worktree(
                WorktreePrepareInput {
                    repo:         &repo,
                    selector:     GitCheckoutSelector::from(&missing_tag),
                    auth:         None,
                    worktree_dir: &temp.path().join("missing-tag"),
                },
                &upstream_url,
            )
            .await
            .expect_err("missing tag should fail");
        assert!(matches!(
            tag_error,
            GitCheckoutError::FetchTag { tag, .. } if tag == "missing"
        ));

        let commit_error = cache
            .prepare_worktree(
                WorktreePrepareInput {
                    repo:         &repo,
                    selector:     GitCheckoutSelector::from(&unavailable_commit),
                    auth:         None,
                    worktree_dir: &temp.path().join("missing-commit"),
                },
                &upstream_url,
            )
            .await
            .expect_err("unavailable commit should fail");
        assert!(matches!(
            commit_error,
            GitCheckoutError::FetchCommit { sha, .. } if sha == unavailable_sha
        ));
    }
}
