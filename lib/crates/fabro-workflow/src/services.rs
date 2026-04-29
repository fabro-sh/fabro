use std::collections::HashMap;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fabro_agent::Sandbox;
use fabro_auth::CredentialSource;
#[cfg(test)]
use fabro_auth::ResolvedCredentials;
use fabro_hooks::{HookContext, HookDecision, HookRunner};
use fabro_model::Provider;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::ManifestPath;
use crate::event::Emitter;
use crate::handler::HandlerRegistry;
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_git::GitState;
use crate::workflow_bundle::WorkflowBundle;

/// Services shared across workflow phases.
#[derive(Clone)]
pub struct RunServices {
    pub run_store:        RunStoreHandle,
    pub emitter:          Arc<Emitter>,
    pub sandbox:          Arc<dyn Sandbox>,
    pub hook_runner:      Option<Arc<HookRunner>>,
    pub cancel_requested: Option<Arc<AtomicBool>>,
    pub provider:         Provider,
    pub llm_source:       Arc<dyn CredentialSource>,
}

impl RunServices {
    #[must_use]
    pub fn new(
        run_store: RunStoreHandle,
        emitter: Arc<Emitter>,
        sandbox: Arc<dyn Sandbox>,
        hook_runner: Option<Arc<HookRunner>>,
        cancel_requested: Option<Arc<AtomicBool>>,
        provider: Provider,
        llm_source: Arc<dyn CredentialSource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            run_store,
            emitter,
            sandbox,
            hook_runner,
            cancel_requested,
            provider,
            llm_source,
        })
    }

    /// Bridge the core executor's atomic cancel flag to sandbox command
    /// cancellation.
    pub fn sandbox_cancel_token(&self) -> Option<CancellationToken> {
        sandbox_cancel_token(self.cancel_requested.clone())
    }

    /// Run lifecycle hooks and return the merged decision.
    /// Returns `Proceed` if no hook runner is configured.
    pub async fn run_hooks(&self, hook_context: &HookContext) -> HookDecision {
        let Some(ref runner) = self.hook_runner else {
            return HookDecision::Proceed;
        };
        runner
            .run(hook_context, Arc::clone(&self.sandbox), None)
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
        Arc::new(Self {
            sandbox,
            ..self.as_ref().clone()
        })
    }

    #[must_use]
    pub fn with_cancel_requested(
        self: &Arc<Self>,
        cancel_requested: Option<Arc<AtomicBool>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            cancel_requested,
            ..self.as_ref().clone()
        })
    }
}

/// Services available only while executing workflow nodes.
pub struct EngineServices {
    pub run:              Arc<RunServices>,
    pub registry:         Arc<HandlerRegistry>,
    /// Git state for the current run. Set via `set_git_state` at the start of
    /// `execute` and read by parallel/fan-in handlers.
    pub(crate) git_state: std::sync::RwLock<Option<Arc<GitState>>>,
    /// Environment variables from `[sandbox.env]` config, injected into command
    /// nodes.
    pub env:              HashMap<String, String>,
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
            async fn resolve(&self) -> anyhow::Result<ResolvedCredentials> {
                Ok(ResolvedCredentials {
                    credentials: Vec::new(),
                    auth_issues: Vec::new(),
                })
            }

            async fn configured_providers(&self) -> Vec<Provider> {
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

        Self {
            run:             RunServices::new(
                run_store.into(),
                Arc::new(Emitter::default()),
                Arc::new(fabro_agent::LocalSandbox::new(
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                )),
                None,
                None,
                Provider::Anthropic,
                Arc::new(StubCredentialSource),
            ),
            registry:        Arc::new(HandlerRegistry::new(Box::new(start::StartHandler))),
            git_state:       std::sync::RwLock::new(None),
            env:             HashMap::new(),
            inputs:          HashMap::new(),
            dry_run:         false,
            workflow_path:   None,
            workflow_bundle: None,
        }
    }
}

pub(crate) fn sandbox_cancel_token(
    cancel_requested: Option<Arc<AtomicBool>>,
) -> Option<CancellationToken> {
    let cancel_requested = cancel_requested?;
    let token = CancellationToken::new();

    if cancel_requested.load(Ordering::Relaxed) {
        token.cancel();
        return Some(token);
    }

    let token_clone = token.clone();
    tokio::spawn(async move {
        loop {
            if token_clone.is_cancelled() {
                return;
            }
            if cancel_requested.load(Ordering::Relaxed) {
                token_clone.cancel();
                return;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    });

    Some(token)
}

#[cfg(test)]
mod tests {
    use super::EngineServices;

    #[tokio::test]
    async fn test_default_uses_stub_credential_source() {
        let services = EngineServices::test_default();

        assert!(
            services
                .run
                .llm_source
                .configured_providers()
                .await
                .is_empty()
        );
    }
}
