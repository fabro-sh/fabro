use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

use fabro_agent::{Sandbox, ToolEnvProvider};
use fabro_auth::CredentialSource;
#[cfg(test)]
use fabro_auth::ResolvedCredentials;
use fabro_hooks::{HookContext, HookDecision, HookExecutionContext, HookRunner};
use fabro_interview::Interviewer;
use fabro_model::{Catalog, ProviderId};
use fabro_types::{ManifestPath, RunId};
use tokio_util::sync::CancellationToken;

use crate::event::Emitter;
use crate::github_token_source::GitHubTokenSource;
use crate::handler::HandlerRegistry;
use crate::interview_runtime::RunInterviewBlocker;
use crate::run_metadata::{RunMetadataRuntime, RunMetadataWriterHandle};
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_git::GitState;
use crate::sandbox_git_runtime::SandboxGitRuntime;
use crate::workflow_bundle::WorkflowBundle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunLocations {
    pub host_source_dir:  Option<PathBuf>,
    pub sandbox_work_dir: Option<PathBuf>,
    pub run_scratch_dir:  PathBuf,
}

impl RunLocations {
    #[must_use]
    pub fn new(
        host_source_dir: Option<PathBuf>,
        sandbox_work_dir: Option<PathBuf>,
        run_scratch_dir: PathBuf,
    ) -> Self {
        Self {
            host_source_dir,
            sandbox_work_dir,
            run_scratch_dir,
        }
    }

    #[must_use]
    pub fn for_sandbox(
        host_source_dir: Option<PathBuf>,
        sandbox: &dyn Sandbox,
        run_scratch_dir: PathBuf,
    ) -> Self {
        Self::new(
            host_source_dir,
            Some(PathBuf::from(sandbox.working_directory())),
            run_scratch_dir,
        )
    }

    #[must_use]
    pub fn hook_execution_context(&self) -> HookExecutionContext {
        HookExecutionContext {
            host_source_dir:  self.host_source_dir.clone(),
            sandbox_work_dir: self.sandbox_work_dir.clone(),
        }
    }

    #[must_use]
    pub fn with_sandbox_work_dir(&self, sandbox_work_dir: Option<PathBuf>) -> Self {
        Self {
            sandbox_work_dir,
            ..self.clone()
        }
    }
}

#[derive(Clone)]
pub struct FabroRunToolServices {
    pub backend:            Arc<dyn fabro_tool::FabroToolBackend>,
    pub current_run_id:     RunId,
    pub base_cwd:           PathBuf,
    pub user_settings_path: PathBuf,
}

/// Services shared across workflow phases.
///
/// Production construction is expected to happen from pipeline initialization
/// with the run's root cancellation token. Use
/// [`RunServices::with_cancel_token`] only with the same root token or a
/// `child_token()` derived from it. The token semantically means "cancel this
/// run or child run," not a generic shutdown signal — dropping a `RunServices`
/// does NOT count as cancellation.
#[derive(Clone)]
pub struct RunServices {
    pub run_store:                RunStoreHandle,
    pub emitter:                  Arc<Emitter>,
    pub sandbox:                  Arc<dyn Sandbox>,
    pub hook_runner:              Option<Arc<HookRunner>>,
    pub locations:                RunLocations,
    pub(crate) cancel_token:      CancellationToken,
    pub provider_id:              ProviderId,
    pub model:                    String,
    pub llm_source:               Arc<dyn CredentialSource>,
    pub catalog:                  Arc<Catalog>,
    pub(crate) sandbox_git:       Arc<SandboxGitRuntime>,
    pub(crate) metadata_runtime:  Arc<RunMetadataRuntime>,
    pub(crate) metadata_writer:   Option<RunMetadataWriterHandle>,
    pub(crate) interview_blocker: Arc<RunInterviewBlocker>,
}

impl RunServices {
    #[must_use]
    pub(crate) fn new(
        run_store: RunStoreHandle,
        emitter: Arc<Emitter>,
        sandbox: Arc<dyn Sandbox>,
        hook_runner: Option<Arc<HookRunner>>,
        locations: RunLocations,
        cancel_token: CancellationToken,
        provider_id: ProviderId,
        model: String,
        llm_source: Arc<dyn CredentialSource>,
        catalog: Arc<Catalog>,
        sandbox_git: Arc<SandboxGitRuntime>,
        metadata_runtime: Arc<RunMetadataRuntime>,
        metadata_writer: Option<RunMetadataWriterHandle>,
    ) -> Arc<Self> {
        Arc::new(Self {
            run_store,
            emitter,
            sandbox,
            hook_runner,
            locations,
            cancel_token,
            provider_id,
            model,
            llm_source,
            catalog,
            sandbox_git,
            metadata_runtime,
            metadata_writer,
            interview_blocker: Arc::new(RunInterviewBlocker::new()),
        })
    }

    /// The run-level cancellation token. Cancel this to terminate the run.
    /// Derive child tokens via `cancel_token().child_token()` for sandbox
    /// command invocations.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Run lifecycle hooks and return the merged decision.
    /// Returns `Proceed` if no hook runner is configured.
    pub async fn run_hooks(&self, hook_context: &HookContext) -> HookDecision {
        let Some(ref runner) = self.hook_runner else {
            return HookDecision::Proceed;
        };
        runner
            .run(
                hook_context,
                Arc::clone(&self.sandbox),
                self.locations.hook_execution_context(),
            )
            .await
    }

    #[must_use]
    pub fn with_run_store(self: &Arc<Self>, run_store: RunStoreHandle) -> Arc<Self> {
        Arc::new(Self {
            run_store,
            ..self.as_ref().clone()
        })
    }

    #[must_use]
    pub fn with_emitter(self: &Arc<Self>, emitter: Arc<Emitter>) -> Arc<Self> {
        Arc::new(Self {
            emitter,
            ..self.as_ref().clone()
        })
    }

    #[must_use]
    pub fn with_sandbox(self: &Arc<Self>, sandbox: Arc<dyn Sandbox>) -> Arc<Self> {
        let locations = self
            .locations
            .with_sandbox_work_dir(Some(PathBuf::from(sandbox.working_directory())));
        Arc::new(Self {
            sandbox,
            locations,
            ..self.as_ref().clone()
        })
    }

    /// Replace the cancellation token. Use only with the same root token or
    /// a child derived from it via `child_token()`.
    #[must_use]
    pub(crate) fn with_cancel_token(
        self: &Arc<Self>,
        cancel_token: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            cancel_token,
            ..self.as_ref().clone()
        })
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_catalog_context(
        self: &Arc<Self>,
        catalog: Arc<Catalog>,
        provider_id: ProviderId,
        model: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            provider_id,
            model,
            catalog,
            ..self.as_ref().clone()
        })
    }
}

/// Services available only while executing workflow nodes.
pub struct EngineServices {
    pub run:              Arc<RunServices>,
    pub registry:         Arc<HandlerRegistry>,
    pub interviewer:      Arc<dyn Interviewer>,
    /// Git state for the current run. Set via `set_git_state` at the start of
    /// `execute` and read by parallel/fan-in handlers.
    pub(crate) git_state: std::sync::RwLock<Option<Arc<GitState>>>,
    /// Environment variables from devcontainer and `[sandbox.env]` config.
    pub base_env:         HashMap<String, String>,
    /// GitHub token source used to inject `GITHUB_TOKEN` at the point of use.
    pub github_token:     Option<Arc<GitHubTokenSource>>,
    /// Typed values from `[run.inputs]`, available to prompt templates.
    pub inputs:           HashMap<String, toml::Value>,
    /// When true, handlers should skip real execution and return simulated
    /// results.
    pub dry_run:          bool,
    /// Manifest path of the current workflow when running from a bundle.
    pub workflow_path:    Option<ManifestPath>,
    /// Bundled workflows available for child-workflow resolution.
    pub workflow_bundle:  Option<Arc<WorkflowBundle>>,
}

impl EngineServices {
    pub async fn env_for_stage(&self) -> anyhow::Result<HashMap<String, String>> {
        resolve_workflow_env(&self.base_env, self.github_token.as_ref()).await
    }

    /// Read the current git state (if any).
    pub fn git_state(&self) -> Option<Arc<GitState>> {
        self.git_state.read().unwrap().clone()
    }

    /// Set the git state for the current run.
    pub fn set_git_state(&self, state: Option<Arc<GitState>>) {
        *self.git_state.write().unwrap() = state;
    }

    /// Test-only default: empty registry and cross-phase services.
    #[cfg(test)]
    #[expect(
        clippy::disallowed_methods,
        reason = "Test scaffolding must build a slate-backed run store from sync code."
    )]
    pub fn test_default() -> Self {
        use fabro_store::Database;
        use object_store::memory::InMemory;

        use crate::handler::start;

        #[derive(Debug, Default)]
        struct StubCredentialSource;

        #[async_trait::async_trait]
        impl CredentialSource for StubCredentialSource {
            async fn resolve(&self, catalog: &Catalog) -> anyhow::Result<ResolvedCredentials> {
                let _ = catalog;
                Ok(ResolvedCredentials {
                    credentials: Vec::new(),
                    auth_issues: Vec::new(),
                })
            }

            async fn configured_providers(&self, catalog: &Catalog) -> Vec<ProviderId> {
                let _ = catalog;
                Vec::new()
            }
        }

        let store = Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ));
        let run_store = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime should initialize")
                .block_on(async {
                    store
                        .create_run(&fabro_types::RunId::new())
                        .await
                        .expect("slate-backed test run store should initialize")
                })
        })
        .join()
        .expect("test run store thread should join");
        let sandbox: Arc<dyn Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ));
        let locations = RunLocations::for_sandbox(None, sandbox.as_ref(), PathBuf::from("."));

        Self {
            run:             RunServices::new(
                run_store.into(),
                Arc::new(Emitter::default()),
                sandbox,
                None,
                locations,
                CancellationToken::new(),
                ProviderId::anthropic(),
                "claude-sonnet-4-6".to_string(),
                Arc::new(StubCredentialSource),
                Arc::new(Catalog::from_builtin().expect("default catalog should build")),
                Arc::new(SandboxGitRuntime::new()),
                Arc::new(RunMetadataRuntime::new()),
                None,
            ),
            registry:        Arc::new(HandlerRegistry::new(Box::new(start::StartHandler))),
            interviewer:     Arc::new(fabro_interview::AutoApproveInterviewer::engine()),
            git_state:       std::sync::RwLock::new(None),
            base_env:        HashMap::new(),
            github_token:    None,
            inputs:          HashMap::new(),
            dry_run:         false,
            workflow_path:   None,
            workflow_bundle: None,
        }
    }
}

pub struct WorkflowToolEnvProvider {
    pub base_env:     HashMap<String, String>,
    pub github_token: Option<Arc<GitHubTokenSource>>,
}

#[async_trait::async_trait]
impl ToolEnvProvider for WorkflowToolEnvProvider {
    async fn resolve(&self) -> anyhow::Result<HashMap<String, String>> {
        resolve_workflow_env(&self.base_env, self.github_token.as_ref()).await
    }
}

async fn resolve_workflow_env(
    base_env: &HashMap<String, String>,
    github_token: Option<&Arc<GitHubTokenSource>>,
) -> anyhow::Result<HashMap<String, String>> {
    let mut env = base_env.clone();
    if let Some(source) = github_token {
        env.insert("GITHUB_TOKEN".to_string(), source.current_token().await?);
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use anyhow::anyhow;
    use fabro_agent::ToolEnvProvider as _;
    use fabro_github::InstallationToken;

    use super::{EngineServices, WorkflowToolEnvProvider};
    use crate::github_token_source::{GitHubTokenSource, IatMinter};

    #[tokio::test]
    async fn test_default_uses_stub_credential_source() {
        let services = EngineServices::test_default();

        assert!(
            services
                .run
                .llm_source
                .configured_providers(&services.run.catalog)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn workflow_tool_env_provider_returns_base_env_without_github_token() {
        let provider = WorkflowToolEnvProvider {
            base_env:     HashMap::from([("FOO".to_string(), "bar".to_string())]),
            github_token: None,
        };

        let env = provider.resolve().await.unwrap();

        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
        assert!(!env.contains_key("GITHUB_TOKEN"));
    }

    #[tokio::test]
    async fn workflow_tool_env_provider_merges_current_github_token() {
        let provider = WorkflowToolEnvProvider {
            base_env:     HashMap::from([("FOO".to_string(), "bar".to_string())]),
            github_token: Some(Arc::new(GitHubTokenSource::pat("ghp_pat".to_string()))),
        };

        let env = provider.resolve().await.unwrap();

        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(env.get("GITHUB_TOKEN").map(String::as_str), Some("ghp_pat"));
    }

    struct FailingMinter;

    #[async_trait::async_trait]
    impl IatMinter for FailingMinter {
        async fn mint(&self) -> anyhow::Result<InstallationToken> {
            Err(anyhow!("GITHUB_TOKEN refresh failed"))
        }
    }

    #[tokio::test]
    async fn workflow_tool_env_provider_propagates_token_refresh_errors() {
        let provider = WorkflowToolEnvProvider {
            base_env:     HashMap::new(),
            github_token: Some(Arc::new(GitHubTokenSource::mintable(Arc::new(
                FailingMinter,
            )))),
        };

        let err = format!("{:#}", provider.resolve().await.unwrap_err());
        assert!(err.contains("GITHUB_TOKEN refresh failed"), "got: {err}");
    }
}
