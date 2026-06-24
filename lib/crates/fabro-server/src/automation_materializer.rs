use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use fabro_api::types::RunManifest;
use fabro_automation::{AutomationId, AutomationTarget, GitHubRepositorySlug};
use fabro_config::{EnvironmentLayer, MergeMap};
use fabro_manifest::ManifestBuildInput;
use fabro_types::{DirtyStatus, GitContext, PreRunPushOutcome, RunId};
use fabro_util::error::collect_chain;
use tokio::{fs, task};

use crate::git_checkout::{
    GitCheckoutError, GitRepoCache, WorktreePrepareInput, github_metadata_url,
    parse_github_repository_slug, resolve_git_auth_config,
};

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
pub(crate) enum RunMaterializeError {
    #[error("invalid repository target: {0}")]
    InvalidTarget(String),
    #[error("failed to clone repository: {0}")]
    CloneFailed(String),
    #[error("failed to resolve workflow: {0}")]
    WorkflowNotFound(String),
    #[error("failed to build run manifest: {0}")]
    Manifest(String),
}

impl From<GitCheckoutError> for RunMaterializeError {
    fn from(value: GitCheckoutError) -> Self {
        match value {
            GitCheckoutError::InvalidTarget(message) => Self::InvalidTarget(message),
            GitCheckoutError::CloneFailed(message) => Self::CloneFailed(message),
        }
    }
}

#[async_trait]
pub(crate) trait AutomationRunMaterializer: Send + Sync {
    async fn materialize(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, RunMaterializeError>;
}

#[derive(Clone)]
pub(crate) struct ProductionAutomationRunMaterializer {
    github_credentials:   Option<fabro_github::GitHubCredentials>,
    github_api_base_url:  String,
    http_client:          Option<fabro_http::HttpClient>,
    environment_defaults: MergeMap<EnvironmentLayer>,
    repo_cache:           Arc<GitRepoCache>,
}

impl ProductionAutomationRunMaterializer {
    pub(crate) fn new(
        github_credentials: Option<fabro_github::GitHubCredentials>,
        github_api_base_url: String,
        http_client: Option<fabro_http::HttpClient>,
        environment_defaults: MergeMap<EnvironmentLayer>,
        repo_cache: Arc<GitRepoCache>,
    ) -> Self {
        Self {
            github_credentials,
            github_api_base_url,
            http_client,
            environment_defaults,
            repo_cache,
        }
    }
}

#[async_trait]
impl AutomationRunMaterializer for ProductionAutomationRunMaterializer {
    async fn materialize(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, RunMaterializeError> {
        let repo = parse_github_repository_slug(&input.target.repository)?;
        fs::create_dir_all(&input.temp_root).await.map_err(|err| {
            RunMaterializeError::CloneFailed(format!(
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
                RunMaterializeError::CloneFailed(format!(
                    "failed to create per-run temp directory under {}: {err}",
                    input.temp_root.display()
                ))
            })?;
        let checkout_dir = temp_dir.path().join("repo");
        let auth = resolve_git_auth_config(
            self.github_credentials.as_ref(),
            &repo,
            &self.github_api_base_url,
            self.http_client.clone(),
        )
        .await
        .map_err(|err| RunMaterializeError::CloneFailed(render_error_chain(err.as_ref())))?;

        let checked_out_sha = self
            .repo_cache
            .prepare_worktree(WorktreePrepareInput {
                repo:         &repo,
                ref_selector: &input.target.ref_selector,
                auth:         auth.as_ref(),
                worktree_dir: &checkout_dir,
            })
            .await?;

        let manifest_input = ManifestFromCheckoutInput {
            workflow: input.target.workflow,
            run_id: input.run_id,
            user_settings_path: input.user_settings_path,
            checkout_dir,
            git_context: ManifestGitContextInput {
                repo,
                ref_selector: input.target.ref_selector,
                checked_out_sha,
            },
            environment_defaults: self.environment_defaults.clone(),
        };
        task::spawn_blocking(move || build_manifest_from_checkout(manifest_input))
            .await
            .map_err(|err| {
                RunMaterializeError::Manifest(format!("manifest build task failed: {err}"))
            })?
    }
}

fn render_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    collect_chain(error).join(": ")
}

#[derive(Debug)]
pub(crate) struct ManifestFromCheckoutInput {
    workflow:             String,
    run_id:               RunId,
    user_settings_path:   PathBuf,
    checkout_dir:         PathBuf,
    git_context:          ManifestGitContextInput,
    environment_defaults: MergeMap<EnvironmentLayer>,
}

#[derive(Debug)]
pub(crate) struct ManifestGitContextInput {
    repo:            GitHubRepositorySlug,
    ref_selector:    String,
    checked_out_sha: String,
}

fn build_manifest_from_checkout(
    args: ManifestFromCheckoutInput,
) -> Result<AutomationRunMaterialized, RunMaterializeError> {
    let ManifestFromCheckoutInput {
        workflow,
        run_id,
        user_settings_path,
        checkout_dir,
        git_context,
        environment_defaults,
    } = args;
    let built = fabro_manifest::build_run_manifest(ManifestBuildInput {
        workflow: workflow.into(),
        cwd: checkout_dir,
        run_id: Some(run_id),
        user_settings_path: Some(user_settings_path),
        environment_defaults,
        ..ManifestBuildInput::default()
    })
    .map_err(|err| manifest_build_error(&err))?;

    let mut manifest = built.manifest;
    manifest.git = Some(GitContext {
        origin_url:   github_metadata_url(&git_context.repo),
        branch:       git_context.ref_selector,
        sha:          Some(git_context.checked_out_sha),
        dirty:        DirtyStatus::Clean,
        push_outcome: PreRunPushOutcome::NotAttempted,
    });
    let submitted_manifest_bytes = serde_json::to_vec(&manifest)
        .context("failed to serialize materialized run manifest")
        .map_err(|err| RunMaterializeError::Manifest(err.to_string()))?;
    Ok(AutomationRunMaterialized {
        manifest,
        submitted_manifest_bytes,
    })
}

fn manifest_build_error(error: &anyhow::Error) -> RunMaterializeError {
    if error.chain().any(|source| {
        source
            .downcast_ref::<fabro_config::Error>()
            .is_some_and(|err| matches!(err, fabro_config::Error::WorkflowNotFound(_)))
    }) {
        RunMaterializeError::WorkflowNotFound(render_error_chain(error.as_ref()))
    } else {
        RunMaterializeError::Manifest(render_error_chain(error.as_ref()))
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
    response:        Result<AutomationRunMaterialized, RunMaterializeError>,
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
        Self::new(Err(RunMaterializeError::InvalidTarget(message.into())))
    }

    fn new(response: Result<AutomationRunMaterialized, RunMaterializeError>) -> Self {
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
    ) -> Result<AutomationRunMaterialized, RunMaterializeError> {
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

    use std::collections::HashMap;
    use std::fs;

    use fabro_types::{DirtyStatus, PreRunPushOutcome, RunId};
    use tempfile::TempDir;

    use super::*;

    fn test_environment_defaults() -> MergeMap<EnvironmentLayer> {
        MergeMap::from(HashMap::from([("default".to_string(), EnvironmentLayer {
            provider: Some("local".to_string()),
            ..EnvironmentLayer::default()
        })]))
    }

    #[test]
    fn manifest_builder_uses_checkout_for_workflow_and_separate_git_context() {
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
        let repo = parse_github_repository_slug("workspace-org/app").unwrap();
        let sha = "0123456789abcdef0123456789abcdef01234567".to_string();

        let materialized = build_manifest_from_checkout(ManifestFromCheckoutInput {
            workflow: "demo".to_string(),
            run_id,
            user_settings_path: user_settings_path.clone(),
            checkout_dir: checkout.clone(),
            git_context: ManifestGitContextInput {
                repo,
                ref_selector: "release".to_string(),
                checked_out_sha: sha.clone(),
            },
            environment_defaults: test_environment_defaults(),
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
        assert_eq!(git.origin_url, "https://github.com/workspace-org/app");
        assert_eq!(git.branch, "release");
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
