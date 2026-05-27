use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_api::types::RunManifest;
use fabro_automation::{AutomationId, AutomationTarget};
use fabro_manifest::ManifestBuildInput;
use fabro_types::{DirtyStatus, GitContext, PreRunPushOutcome, RunId};
use fabro_util::error::collect_chain;
use tokio::process::Command;
use tokio::{fs, task, time};

const GIT_CLONE_TIMEOUT: Duration = Duration::from_mins(2);
const GIT_FETCH_TIMEOUT: Duration = Duration::from_mins(1);
const GIT_CHECKOUT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_REV_PARSE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutomationRunMaterializeInput {
    pub automation_id:      AutomationId,
    pub target:             AutomationTarget,
    pub run_id:             RunId,
    pub user_settings_path: PathBuf,
    pub temp_root:          PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct AutomationRunMaterialized {
    pub manifest:                 RunManifest,
    pub submitted_manifest_bytes: Vec<u8>,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutomationRunMaterializeError {
    #[error("invalid automation target: {0}")]
    InvalidTarget(String),
    #[error("failed to clone automation repository: {0}")]
    CloneFailed(String),
    #[error("failed to resolve automation workflow: {0}")]
    WorkflowNotFound(String),
    #[error("failed to build run manifest: {0}")]
    Manifest(String),
}

#[async_trait]
pub(crate) trait AutomationRunMaterializer: Send + Sync {
    async fn materialize(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, AutomationRunMaterializeError>;
}

#[derive(Clone)]
pub(crate) struct ProductionAutomationRunMaterializer {
    github_credentials:  Option<fabro_github::GitHubCredentials>,
    github_api_base_url: String,
    http_client:         Option<fabro_http::HttpClient>,
}

impl ProductionAutomationRunMaterializer {
    pub(crate) fn new(
        github_credentials: Option<fabro_github::GitHubCredentials>,
        github_api_base_url: String,
        http_client: Option<fabro_http::HttpClient>,
    ) -> Self {
        Self {
            github_credentials,
            github_api_base_url,
            http_client,
        }
    }
}

#[async_trait]
impl AutomationRunMaterializer for ProductionAutomationRunMaterializer {
    async fn materialize(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, AutomationRunMaterializeError> {
        let repo = parse_github_repository_slug(&input.target.repository)?;
        fs::create_dir_all(&input.temp_root).await.map_err(|err| {
            AutomationRunMaterializeError::CloneFailed(format!(
                "failed to create temp root {}: {err}",
                input.temp_root.display()
            ))
        })?;
        let temp_dir = tempfile::Builder::new()
            .prefix(&format!(
                "automation-{}-{}-",
                input.automation_id.as_str(),
                input.run_id
            ))
            .tempdir_in(&input.temp_root)
            .map_err(|err| {
                AutomationRunMaterializeError::CloneFailed(format!(
                    "failed to create per-run temp directory under {}: {err}",
                    input.temp_root.display()
                ))
            })?;
        let checkout_dir = temp_dir.path().join("repo");
        let clone_url = github_clone_url(&repo);
        let auth = resolve_git_auth_config(
            self.github_credentials.as_ref(),
            &repo,
            &self.github_api_base_url,
            self.http_client.clone(),
        )
        .await
        .map_err(|err| {
            AutomationRunMaterializeError::CloneFailed(render_error_chain(err.as_ref()))
        })?;

        run_git_plan(build_clone_plan(&clone_url, &checkout_dir, auth.as_ref())).await?;
        run_git_plan(build_fetch_ref_plan(
            &clone_url,
            &checkout_dir,
            &input.target.ref_selector,
            auth.as_ref(),
        ))
        .await?;
        run_git_plan(build_checkout_ref_plan(&checkout_dir)).await?;
        let checked_out_sha = run_git_plan(build_rev_parse_head_plan(&checkout_dir))
            .await
            .map(|stdout| String::from_utf8_lossy(&stdout).trim().to_string())?;

        let manifest_input = ManifestFromCheckoutInput {
            input,
            checkout_dir,
            repo,
            checked_out_sha: Some(checked_out_sha),
        };
        task::spawn_blocking(move || build_manifest_from_checkout(manifest_input))
            .await
            .map_err(|err| {
                AutomationRunMaterializeError::Manifest(format!(
                    "manifest build task failed: {err}"
                ))
            })?
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubRepository {
    owner: String,
    name:  String,
}

fn parse_github_repository_slug(
    value: &str,
) -> Result<GithubRepository, AutomationRunMaterializeError> {
    let Some((owner, repo)) = value.split_once('/') else {
        return Err(AutomationRunMaterializeError::InvalidTarget(format!(
            "repository must be a GitHub owner/repo slug: {value}"
        )));
    };
    if repo.contains('/') || !valid_github_owner(owner) || !valid_github_repo(repo) {
        return Err(AutomationRunMaterializeError::InvalidTarget(format!(
            "repository must be a GitHub owner/repo slug: {value}"
        )));
    }
    Ok(GithubRepository {
        owner: owner.to_string(),
        name:  repo.to_string(),
    })
}

fn valid_github_owner(value: &str) -> bool {
    if value.is_empty() || value.len() > 39 {
        return false;
    }
    let bytes = value.as_bytes();
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    (first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
}

fn valid_github_repo(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn github_clone_url(repo: &GithubRepository) -> String {
    format!("https://github.com/{}/{}.git", repo.owner, repo.name)
}

fn github_metadata_url(repo: &GithubRepository) -> String {
    format!("https://github.com/{}/{}", repo.owner, repo.name)
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

async fn resolve_git_auth_config(
    credentials: Option<&fabro_github::GitHubCredentials>,
    repo: &GithubRepository,
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
        fabro_github::resolve_clone_credentials(&context, &repo.owner, &repo.name).await?;
    Ok(Some(GitAuthConfig::new(username, password)))
}

fn basic_auth_header(username: &str, password: &str) -> String {
    basic_auth_header_from_encoded(&BASE64_STANDARD.encode(format!("{username}:{password}")))
}

fn basic_auth_header_from_encoded(encoded_credentials: &str) -> String {
    format!("AUTHORIZATION: basic {encoded_credentials}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitCommandPlan {
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

    fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn build_clone_plan(
    clone_url: &str,
    checkout_dir: &Path,
    auth: Option<&GitAuthConfig>,
) -> GitCommandPlan {
    GitCommandPlan::new(
        [
            "clone".to_string(),
            "--depth".to_string(),
            "1".to_string(),
            "--no-checkout".to_string(),
            clone_url.to_string(),
            checkout_dir.display().to_string(),
        ],
        GIT_CLONE_TIMEOUT,
    )
    .with_auth(clone_url, auth)
}

fn build_fetch_ref_plan(
    clone_url: &str,
    checkout_dir: &Path,
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
    .current_dir(checkout_dir)
    .with_auth(clone_url, auth)
}

fn build_checkout_ref_plan(checkout_dir: &Path) -> GitCommandPlan {
    GitCommandPlan::new(
        ["checkout", "--force", "--detach", "FETCH_HEAD"],
        GIT_CHECKOUT_TIMEOUT,
    )
    .current_dir(checkout_dir)
}

fn build_rev_parse_head_plan(checkout_dir: &Path) -> GitCommandPlan {
    GitCommandPlan::new(["rev-parse", "HEAD"], GIT_REV_PARSE_TIMEOUT).current_dir(checkout_dir)
}

async fn run_git_plan(plan: GitCommandPlan) -> Result<Vec<u8>, AutomationRunMaterializeError> {
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
            AutomationRunMaterializeError::CloneFailed(format!(
                "{} timed out after {}s",
                safe_command_label(&plan),
                plan.timeout.as_secs()
            ))
        })?
        .map_err(|err| {
            AutomationRunMaterializeError::CloneFailed(format!(
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
    Err(AutomationRunMaterializeError::CloneFailed(
        redact_git_output(&message, &plan.sensitive_values),
    ))
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

fn render_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    collect_chain(error).join(": ")
}

#[derive(Debug)]
pub(crate) struct ManifestFromCheckoutInput {
    input:           AutomationRunMaterializeInput,
    checkout_dir:    PathBuf,
    repo:            GithubRepository,
    checked_out_sha: Option<String>,
}

fn build_manifest_from_checkout(
    args: ManifestFromCheckoutInput,
) -> Result<AutomationRunMaterialized, AutomationRunMaterializeError> {
    let ManifestFromCheckoutInput {
        input,
        checkout_dir,
        repo,
        checked_out_sha,
    } = args;
    let built = fabro_manifest::build_run_manifest(ManifestBuildInput {
        workflow: input.target.workflow.as_str().into(),
        cwd: checkout_dir,
        run_id: Some(input.run_id),
        user_settings_path: Some(input.user_settings_path),
        ..ManifestBuildInput::default()
    })
    .map_err(|err| manifest_build_error(&err))?;

    let mut manifest = built.manifest;
    manifest.git = Some(GitContext {
        origin_url:   github_metadata_url(&repo),
        branch:       input.target.ref_selector,
        sha:          checked_out_sha,
        dirty:        DirtyStatus::Clean,
        push_outcome: PreRunPushOutcome::NotAttempted,
    });
    let submitted_manifest_bytes = serde_json::to_vec(&manifest)
        .with_context(|| {
            format!(
                "failed to serialize materialized manifest for automation {}",
                input.automation_id.as_str()
            )
        })
        .map_err(|err| AutomationRunMaterializeError::Manifest(err.to_string()))?;
    Ok(AutomationRunMaterialized {
        manifest,
        submitted_manifest_bytes,
    })
}

fn manifest_build_error(error: &anyhow::Error) -> AutomationRunMaterializeError {
    if error.chain().any(|source| {
        source
            .downcast_ref::<fabro_config::Error>()
            .is_some_and(|err| matches!(err, fabro_config::Error::WorkflowNotFound(_)))
    }) {
        AutomationRunMaterializeError::WorkflowNotFound(render_error_chain(error.as_ref()))
    } else {
        AutomationRunMaterializeError::Manifest(render_error_chain(error.as_ref()))
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
pub struct TestAutomationRunMaterializer {
    inner: std::sync::Arc<std::sync::Mutex<TestAutomationRunMaterializerState>>,
}

#[cfg(any(test, feature = "test-support"))]
struct TestAutomationRunMaterializerState {
    captured_inputs: Vec<AutomationRunMaterializeInput>,
    response:        Result<AutomationRunMaterialized, AutomationRunMaterializeError>,
}

#[cfg(any(test, feature = "test-support"))]
impl TestAutomationRunMaterializer {
    pub fn succeed(manifest: RunManifest, submitted_manifest_bytes: Vec<u8>) -> Self {
        Self::new(Ok(AutomationRunMaterialized {
            manifest,
            submitted_manifest_bytes,
        }))
    }

    pub fn fail_invalid_target(message: impl Into<String>) -> Self {
        Self::new(Err(AutomationRunMaterializeError::InvalidTarget(
            message.into(),
        )))
    }

    fn new(response: Result<AutomationRunMaterialized, AutomationRunMaterializeError>) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(TestAutomationRunMaterializerState {
                captured_inputs: Vec::new(),
                response,
            })),
        }
    }

    pub(crate) fn captured_inputs(&self) -> Vec<AutomationRunMaterializeInput> {
        self.inner
            .lock()
            .expect("test automation materializer lock poisoned")
            .captured_inputs
            .clone()
    }

    pub(crate) fn into_materializer(self) -> std::sync::Arc<dyn AutomationRunMaterializer> {
        std::sync::Arc::new(self)
    }
}

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl AutomationRunMaterializer for TestAutomationRunMaterializer {
    async fn materialize(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, AutomationRunMaterializeError> {
        let mut guard = self
            .inner
            .lock()
            .expect("test automation materializer lock poisoned");
        guard.captured_inputs.push(input);
        guard.response.clone()
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "Materializer unit tests write small temporary workflow fixtures synchronously."
    )]

    use std::fs;
    use std::path::Path;

    use fabro_automation::{AutomationId, AutomationTarget};
    use fabro_types::{DirtyStatus, PreRunPushOutcome, RunId};
    use tempfile::TempDir;

    use super::*;

    fn target(repository: &str, ref_selector: &str, workflow: &str) -> AutomationTarget {
        AutomationTarget {
            repository:   repository.to_string(),
            ref_selector: ref_selector.to_string(),
            workflow:     workflow.to_string(),
        }
    }

    #[test]
    fn target_repository_urls_are_github_metadata_urls_without_credentials() {
        let repo = parse_github_repository_slug("fabro-sh/fabro").expect("slug should parse");

        assert_eq!(repo.owner, "fabro-sh");
        assert_eq!(repo.name, "fabro");
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
            "owner/.git",
        ] {
            let error = parse_github_repository_slug(value).expect_err("invalid slug should fail");
            assert!(
                error.to_string().contains("invalid automation target"),
                "unexpected error for {value}: {error}"
            );
        }
    }

    #[test]
    fn ref_checkout_command_plans_use_argv_prompt_disable_and_timeouts() {
        let repo = parse_github_repository_slug("fabro-sh/fabro").unwrap();
        let clone_url = github_clone_url(&repo);
        let temp = TempDir::new().unwrap();
        let checkout_dir = temp.path().join("repo");

        let clone = build_clone_plan(&clone_url, &checkout_dir, None);
        assert_eq!(clone.program, "git");
        assert_eq!(clone.args, vec![
            "clone",
            "--depth",
            "1",
            "--no-checkout",
            "https://github.com/fabro-sh/fabro.git",
            checkout_dir.to_str().unwrap(),
        ]);
        assert_eq!(clone.timeout, Duration::from_mins(2));
        assert_eq!(clone.env_value("GIT_TERMINAL_PROMPT"), Some("0"));

        let fetch = build_fetch_ref_plan(&clone_url, &checkout_dir, "feature/materialize", None);
        assert_eq!(fetch.args, vec![
            "fetch",
            "--depth",
            "1",
            "origin",
            "--",
            "feature/materialize",
        ]);
        assert_eq!(fetch.current_dir.as_deref(), Some(checkout_dir.as_path()));
        assert_eq!(fetch.timeout, Duration::from_mins(1));
        assert_eq!(fetch.env_value("GIT_TERMINAL_PROMPT"), Some("0"));

        let checkout = build_checkout_ref_plan(&checkout_dir);
        assert_eq!(checkout.args, vec![
            "checkout",
            "--force",
            "--detach",
            "FETCH_HEAD"
        ]);
        assert_eq!(
            checkout.current_dir.as_deref(),
            Some(checkout_dir.as_path())
        );
        assert_eq!(checkout.timeout, Duration::from_secs(30));
        assert_eq!(checkout.env_value("GIT_TERMINAL_PROMPT"), Some("0"));
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
        let plan = build_clone_plan(&clone_url, Path::new("/tmp/fabro-checkout"), Some(&auth));

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

    #[test]
    fn workflow_path_resolution_builds_manifest_from_checkout_directory() {
        let temp = TempDir::new().unwrap();
        let checkout = temp.path().join("checkout");
        let workflow_dir = checkout.join(".fabro/workflows/demo");
        fs::create_dir_all(&workflow_dir).unwrap();
        fs::write(checkout.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        fs::write(
            workflow_dir.join("workflow.fabro"),
            r#"digraph Demo { graph [goal="Ship automation"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        let user_settings_path = temp.path().join("settings.toml");
        fs::write(&user_settings_path, "_version = 1\n").unwrap();
        let run_id = RunId::new();
        let repo = parse_github_repository_slug("fabro-sh/fabro").unwrap();
        let sha = "0123456789abcdef0123456789abcdef01234567".to_string();

        let materialized = build_manifest_from_checkout(ManifestFromCheckoutInput {
            input: AutomationRunMaterializeInput {
                automation_id: AutomationId::new("nightly").unwrap(),
                target: target("fabro-sh/fabro", "main", "demo"),
                run_id,
                user_settings_path: user_settings_path.clone(),
                temp_root: temp.path().to_path_buf(),
            },
            checkout_dir: checkout.clone(),
            repo,
            checked_out_sha: Some(sha.clone()),
        })
        .expect("manifest should build from checkout");

        assert_eq!(
            materialized.manifest.run_id.as_deref(),
            Some(run_id.to_string().as_str())
        );
        assert_eq!(materialized.manifest.cwd, checkout.display().to_string());
        assert_eq!(
            materialized.manifest.target.path,
            ".fabro/workflows/demo/workflow.fabro"
        );
        assert!(
            materialized
                .manifest
                .configs
                .iter()
                .any(|config| config.path.as_deref() == Some(user_settings_path.to_str().unwrap()))
        );
        let git = materialized
            .manifest
            .git
            .as_ref()
            .expect("git context should be set");
        assert_eq!(git.origin_url, "https://github.com/fabro-sh/fabro");
        assert_eq!(git.branch, "main");
        assert_eq!(git.sha.as_deref(), Some(sha.as_str()));
        assert_eq!(git.dirty, DirtyStatus::Clean);
        assert_eq!(git.push_outcome, PreRunPushOutcome::NotAttempted);
        let submitted_manifest: serde_json::Value =
            serde_json::from_slice(&materialized.submitted_manifest_bytes)
                .expect("submitted bytes should be a manifest");
        assert_eq!(
            submitted_manifest,
            serde_json::to_value(&materialized.manifest).unwrap()
        );
    }
}
