use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_automation::GitHubRepositorySlug;
use fabro_store::KeyedMutex;
use tokio::process::Command;
use tokio::{fs, time};

const GIT_CLONE_TIMEOUT: Duration = Duration::from_mins(2);
const GIT_FETCH_TIMEOUT: Duration = Duration::from_mins(1);
const GIT_WORKTREE_ADD_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_WORKTREE_PRUNE_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_REV_PARSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Error returned while preparing a checkout from a git source.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitCheckoutError {
    #[error("invalid repository target: {0}")]
    InvalidTarget(String),
    #[error("failed to clone repository: {0}")]
    CloneFailed(String),
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

    /// Prepare a worktree containing the requested ref of `repo` at
    /// `worktree_dir`. Returns the resolved commit SHA.
    ///
    /// First call for a repo: a `--bare --depth 1` clone is created at
    /// `<cache>/<owner>/<repo>.git`. Subsequent calls reuse the bare clone
    /// and only `git fetch --depth 1` the requested ref. In both cases a
    /// short-lived worktree is added at `worktree_dir`; the caller owns its
    /// lifetime (typically a `TempDir`). Stale worktree admin entries from
    /// crashed prior calls are pruned at the start of each call so they do
    /// not accumulate.
    pub(crate) async fn prepare_worktree(
        &self,
        args: WorktreePrepareInput<'_>,
    ) -> Result<String, GitCheckoutError> {
        let clone_url = github_clone_url(args.repo);
        self.prepare_worktree_with_clone_url(args, &clone_url).await
    }

    async fn prepare_worktree_with_clone_url(
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
                let _ = fs::remove_dir_all(&bare_dir).await;
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
        if bare_exists {
            // Drop any worktree admin entries whose working trees were
            // deleted by previous `TempDir` cleanup. Cheap and idempotent.
            let _ = run_git_plan(build_worktree_prune_plan(bare_dir)).await;
        } else {
            if let Some(parent) = bare_dir.parent() {
                fs::create_dir_all(parent).await.map_err(|err| {
                    GitCheckoutError::CloneFailed(format!(
                        "failed to create cache dir {}: {err}",
                        parent.display()
                    ))
                })?;
            }
            run_git_plan(build_bare_clone_plan(clone_url, bare_dir, args.auth)).await?;
        }

        run_git_plan(build_bare_fetch_plan(
            bare_dir,
            clone_url,
            args.ref_selector,
            args.auth,
        ))
        .await?;

        let checked_out_sha = run_git_plan(build_rev_parse_fetch_head_plan(bare_dir))
            .await
            .map(|stdout| String::from_utf8_lossy(&stdout).trim().to_string())?;

        run_git_plan(build_worktree_add_plan(bare_dir, args.worktree_dir)).await?;

        Ok(checked_out_sha)
    }
}

pub(crate) struct WorktreePrepareInput<'a> {
    pub repo:         &'a GitHubRepositorySlug,
    pub ref_selector: &'a str,
    pub auth:         Option<&'a GitAuthConfig>,
    pub worktree_dir: &'a Path,
}

async fn bare_clone_may_be_corrupt(bare_dir: &Path) -> bool {
    match fs::metadata(&bare_dir.join("HEAD")).await {
        Ok(meta) => meta.len() == 0,
        Err(_) => bare_dir.exists(),
    }
}

pub(crate) fn parse_github_repository_slug(
    value: &str,
) -> Result<GitHubRepositorySlug, GitCheckoutError> {
    fabro_automation::parse_github_repository_slug(value)
        .map_err(|err| GitCheckoutError::InvalidTarget(err.to_string()))
}

fn github_clone_url(repo: &GitHubRepositorySlug) -> String {
    format!("https://github.com/{}/{}.git", repo.owner(), repo.repo())
}

pub(crate) fn github_metadata_url(repo: &GitHubRepositorySlug) -> String {
    format!("https://github.com/{}/{}", repo.owner(), repo.repo())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitAuthConfig {
    extraheader:      Option<String>,
    sensitive_values: Vec<String>,
}

impl GitAuthConfig {
    fn new(username: Option<String>, password: Option<String>) -> Self {
        let Some(password) = password.filter(|value| !value.is_empty()) else {
            return Self {
                extraheader:      None,
                sensitive_values: Vec::new(),
            };
        };
        let username = username
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "x-access-token".to_string());
        let encoded_credentials = BASE64_STANDARD.encode(format!("{username}:{password}"));
        let extraheader = basic_auth_header_from_encoded(&encoded_credentials);
        Self {
            sensitive_values: vec![password, encoded_credentials, extraheader.clone()],
            extraheader:      Some(extraheader),
        }
    }

    fn git_env(&self, clone_url: &str) -> Vec<(String, String)> {
        let Some(extraheader) = self.extraheader.as_ref() else {
            return Vec::new();
        };
        vec![
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            (
                "GIT_CONFIG_KEY_0".to_string(),
                format!("http.{clone_url}.extraheader"),
            ),
            ("GIT_CONFIG_VALUE_0".to_string(), extraheader.clone()),
        ]
    }

    fn sensitive_values(&self) -> &[String] {
        &self.sensitive_values
    }
}

pub(crate) async fn resolve_git_auth_config(
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
    let (username, password) =
        fabro_github::resolve_clone_credentials(&context, repo.owner(), repo.repo()).await?;
    Ok(Some(GitAuthConfig::new(username, password)))
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

fn build_worktree_add_plan(bare_dir: &Path, worktree_dir: &Path) -> GitCommandPlan {
    GitCommandPlan::new(
        [
            "worktree".to_string(),
            "add".to_string(),
            "--detach".to_string(),
            "--force".to_string(),
            worktree_dir.display().to_string(),
            "FETCH_HEAD".to_string(),
        ],
        GIT_WORKTREE_ADD_TIMEOUT,
    )
    .current_dir(bare_dir)
}

fn build_worktree_prune_plan(bare_dir: &Path) -> GitCommandPlan {
    GitCommandPlan::new(["worktree", "prune"], GIT_WORKTREE_PRUNE_TIMEOUT).current_dir(bare_dir)
}

fn build_rev_parse_fetch_head_plan(bare_dir: &Path) -> GitCommandPlan {
    GitCommandPlan::new(["rev-parse", "FETCH_HEAD"], GIT_REV_PARSE_TIMEOUT).current_dir(bare_dir)
}

async fn run_git_plan(plan: GitCommandPlan) -> Result<Vec<u8>, GitCheckoutError> {
    let mut command = Command::new(&plan.program);
    command.args(&plan.args);
    command.envs(plan.env.iter().map(|(key, value)| (key, value)));
    if let Some(current_dir) = plan.current_dir.as_ref() {
        command.current_dir(current_dir);
    }
    command.kill_on_drop(true);

    let output = time::timeout(plan.timeout, command.output())
        .await
        .map_err(|_| {
            GitCheckoutError::CloneFailed(format!(
                "{} timed out after {}s",
                safe_command_label(&plan),
                plan.timeout.as_secs()
            ))
        })?
        .map_err(|err| {
            GitCheckoutError::CloneFailed(format!(
                "failed to run {}: {err}",
                safe_command_label(&plan)
            ))
        })?;

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
    Err(GitCheckoutError::CloneFailed(redact_git_output(
        &message,
        &plan.sensitive_values,
    )))
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

    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn target_repository_urls_are_github_metadata_urls_without_credentials() {
        let repo = parse_github_repository_slug("fabro-sh/fabro").expect("slug should parse");

        assert_eq!(repo.owner(), "fabro-sh");
        assert_eq!(repo.repo(), "fabro");
        assert_eq!(
            github_clone_url(&repo),
            "https://github.com/fabro-sh/fabro.git"
        );
        assert_eq!(
            github_metadata_url(&repo),
            "https://github.com/fabro-sh/fabro"
        );
        assert!(!github_clone_url(&repo).contains('@'));
    }

    #[test]
    fn target_repository_validation_rejects_non_github_owner_repo_shapes() {
        for value in [
            "fabro-sh",
            "https://github.com/fabro-sh/fabro",
            "fabro-sh/fabro/extra",
            "-owner/repo",
        ] {
            let error = parse_github_repository_slug(value).expect_err("invalid slug should fail");
            assert!(
                error.to_string().contains("invalid repository target"),
                "unexpected error for {value}: {error}"
            );
        }
    }

    #[test]
    fn target_repository_validation_matches_automation_validation() {
        let repo = parse_github_repository_slug("owner/.github").expect("slug should parse");

        assert_eq!(repo.owner(), "owner");
        assert_eq!(repo.repo(), ".github");
        assert_eq!(
            github_metadata_url(&repo),
            "https://github.com/owner/.github"
        );
    }

    #[test]
    fn bare_cache_command_plans_use_argv_prompt_disable_and_timeouts() {
        let repo = parse_github_repository_slug("fabro-sh/fabro").unwrap();
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

        let worktree = build_worktree_add_plan(&bare_dir, &worktree_dir);
        assert_eq!(worktree.args, vec![
            "worktree",
            "add",
            "--detach",
            "--force",
            worktree_dir.to_str().unwrap(),
            "FETCH_HEAD",
        ]);
        assert_eq!(worktree.current_dir.as_deref(), Some(bare_dir.as_path()));
        assert_eq!(worktree.timeout, Duration::from_secs(30));
        assert_eq!(worktree.env_value("GIT_TERMINAL_PROMPT"), Some("0"));

        let prune = build_worktree_prune_plan(&bare_dir);
        assert_eq!(prune.args, vec!["worktree", "prune"]);
        assert_eq!(prune.current_dir.as_deref(), Some(bare_dir.as_path()));
        assert_eq!(prune.timeout, Duration::from_secs(10));

        let rev_parse = build_rev_parse_fetch_head_plan(&bare_dir);
        assert_eq!(rev_parse.args, vec!["rev-parse", "FETCH_HEAD"]);
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
        let repo = parse_github_repository_slug("fabro-sh/fabro").unwrap();
        let clone_url = github_clone_url(&repo);
        let auth = GitAuthConfig::new(
            Some("x-access-token".to_string()),
            Some("ghu_secret".to_string()),
        );
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
            .args([
                "-C",
                work.to_str().unwrap(),
                "push",
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
    async fn bare_clone_reused_across_calls() {
        let temp = TempDir::new().unwrap();
        let upstream = temp.path().join("upstream.git");
        let expected_sha = seed_upstream(&upstream);
        let cache = GitRepoCache::new(temp.path().join("cache"));
        let repo = parse_github_repository_slug("fabro-sh/fabro").unwrap();
        let upstream_url = upstream.to_str().unwrap().to_string();

        let worktree_a = temp.path().join("wt-a");
        let sha_a = cache
            .prepare_worktree_with_clone_url(
                WorktreePrepareInput {
                    repo:         &repo,
                    ref_selector: "main",
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
            .prepare_worktree_with_clone_url(
                WorktreePrepareInput {
                    repo:         &repo,
                    ref_selector: "main",
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
        let repo = parse_github_repository_slug("fabro-sh/fabro").unwrap();
        let upstream_url = upstream.to_str().unwrap().to_string();

        let worktree_a = temp.path().join("wt-a");
        cache
            .prepare_worktree_with_clone_url(
                WorktreePrepareInput {
                    repo:         &repo,
                    ref_selector: "main",
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
            .prepare_worktree_with_clone_url(
                WorktreePrepareInput {
                    repo:         &repo,
                    ref_selector: "main",
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
}
