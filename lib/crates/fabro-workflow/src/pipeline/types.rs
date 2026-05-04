use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_graphviz::graph::Graph;
use fabro_interview::Interviewer;
use fabro_llm::Provider;
use fabro_mcp::config::McpServerSettings;
use fabro_model::FallbackTarget;
use fabro_retro::retro::Retro;
use fabro_sandbox::SandboxSpec;
use fabro_sandbox::config::WorktreeMode;
use fabro_types::RunId;
use fabro_types::settings::run::PullRequestSettings;
use fabro_validate::{Diagnostic, Severity};
use fabro_vault::Vault;
use tokio::sync::RwLock as AsyncRwLock;

use crate::ManifestPath;
use crate::artifact_upload::ArtifactSink;
use crate::steering_hub::SteeringHub;
use crate::context::Context;
use crate::error::Error;
use crate::event::Emitter;
use crate::file_resolver::FileResolver;
use crate::handler::HandlerRegistry;
use crate::outcome::Outcome;
use crate::records::{Checkpoint, Conclusion, RunSpec};
use crate::run_control::RunControlState;
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions};
use crate::runtime_store::RunStoreHandle;
use crate::services::{EngineServices, RunServices};
use crate::transforms::Transform;
use crate::workflow_bundle::WorkflowBundle;

/// Output of the PARSE phase.
#[non_exhaustive]
pub struct Parsed {
    pub graph:  Graph,
    pub source: String,
}

/// Output of the TRANSFORM phase. Graph is mutable — callers may apply
/// post-transform adjustments (e.g. goal override) before validation.
#[non_exhaustive]
pub struct Transformed {
    pub graph:  Graph,
    pub source: String,
}

/// Output of the VALIDATE phase. Always produced (even with errors).
/// Caller inspects diagnostics and decides whether to proceed.
/// Graph is read-only — use accessors, not direct field access.
#[non_exhaustive]
pub struct Validated {
    graph:       Graph,
    source:      String,
    diagnostics: Vec<Diagnostic>,
}

impl Validated {
    /// Create a new `Validated` from its parts.
    pub(crate) fn new(graph: Graph, source: String, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            graph,
            source,
            diagnostics,
        }
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// True if any diagnostic has Error severity.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Returns `Err(Error::Validation)` if any Error-severity diagnostics
    /// exist. Diagnostics remain accessible via `diagnostics()` for
    /// printing before this call.
    pub fn raise_on_errors(&self) -> Result<(), Error> {
        if self.has_errors() {
            let message = self
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::Validation(message));
        }
        Ok(())
    }

    /// Consume into owned graph, source, and diagnostics (used by initialize).
    pub fn into_parts(self) -> (Graph, String, Vec<Diagnostic>) {
        (self.graph, self.source, self.diagnostics)
    }
}

/// Options for the PERSIST phase.
pub(crate) struct PersistOptions {
    pub run_dir:  PathBuf,
    pub run_spec: RunSpec,
}

/// Output of the PERSIST phase. Run directory created and the validated
/// workflow is persisted into the durable run spec.
#[derive(Debug)]
#[non_exhaustive]
pub struct Persisted {
    graph:       Graph,
    source:      String,
    diagnostics: Vec<Diagnostic>,
    run_dir:     PathBuf,
    run_spec:    RunSpec,
}

impl Persisted {
    /// Create a new `Persisted` from its parts.
    pub(crate) fn new(
        graph: Graph,
        source: String,
        diagnostics: Vec<Diagnostic>,
        run_dir: PathBuf,
        run_spec: RunSpec,
    ) -> Self {
        Self {
            graph,
            source,
            diagnostics,
            run_dir,
            run_spec,
        }
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    pub fn run_spec(&self) -> &RunSpec {
        &self.run_spec
    }

    /// True if any diagnostic has Error severity.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Returns `Err(Error::Validation)` if any Error-severity diagnostics
    /// exist.
    pub fn raise_on_errors(&self) -> Result<(), Error> {
        if self.has_errors() {
            let message = self
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::Validation(message));
        }
        Ok(())
    }

    /// Consume into owned graph, source, diagnostics, run dir, and run spec.
    pub fn into_parts(self) -> (Graph, String, Vec<Diagnostic>, PathBuf, RunSpec) {
        (
            self.graph,
            self.source,
            self.diagnostics,
            self.run_dir,
            self.run_spec,
        )
    }

    pub async fn load_from_store(
        run_store: &RunStoreHandle,
        run_dir: &Path,
    ) -> Result<Self, Error> {
        super::persist::load_from_store(run_store, run_dir).await
    }
}

#[derive(Clone)]
pub struct LlmSpec {
    pub model:          String,
    pub provider:       Provider,
    pub fallback_chain: Vec<FallbackTarget>,
    pub mcp_servers:    Vec<McpServerSettings>,
    pub dry_run:        bool,
}

#[derive(Clone)]
pub struct SandboxEnvSpec {
    pub devcontainer_env:   HashMap<String, String>,
    pub toml_env:           HashMap<String, String>,
    pub github_permissions: Option<HashMap<String, String>>,
    pub origin_url:         Option<String>,
}

#[derive(Clone)]
pub struct DevcontainerSpec {
    pub enabled:     bool,
    pub resolve_dir: PathBuf,
}

pub struct InitOptions {
    pub run_id:            RunId,
    pub run_store:         RunStoreHandle,
    pub dry_run:           bool,
    pub emitter:           Arc<Emitter>,
    pub sandbox:           SandboxSpec,
    pub llm:               LlmSpec,
    pub interviewer:       Arc<dyn Interviewer>,
    pub lifecycle:         LifecycleOptions,
    pub run_options:       RunOptions,
    pub workflow_path:     Option<ManifestPath>,
    pub workflow_bundle:   Option<Arc<WorkflowBundle>>,
    pub hooks:             fabro_hooks::HookSettings,
    pub sandbox_env:       SandboxEnvSpec,
    pub vault:             Option<Arc<AsyncRwLock<Vault>>>,
    pub devcontainer:      Option<DevcontainerSpec>,
    pub git:               Option<GitCheckpointOptions>,
    pub worktree_mode:     Option<WorktreeMode>,
    pub registry_override: Option<Arc<HandlerRegistry>>,
    pub artifact_sink:     Option<ArtifactSink>,
    pub run_control:       Option<Arc<RunControlState>>,
    pub checkpoint:        Option<Checkpoint>,
    pub seed_context:      Option<Context>,
    pub steering_hub:      Option<Arc<SteeringHub>>,
}

/// Output of the INITIALIZE phase.
#[non_exhaustive]
pub struct Initialized {
    pub graph:               Graph,
    pub source:              String,
    pub run_options:         RunOptions,
    pub(crate) checkpoint:   Option<Checkpoint>,
    pub(crate) seed_context: Option<Context>,
    pub on_node:             crate::OnNodeCallback,
    pub artifact_sink:       Option<ArtifactSink>,
    pub run_control:         Option<Arc<RunControlState>>,
    pub engine:              Arc<EngineServices>,
    pub model:               String,
}

/// Output of the EXECUTE phase.
#[non_exhaustive]
pub struct Executed {
    pub graph:         Graph,
    pub outcome:       Result<Outcome, Error>,
    pub run_options:   RunOptions,
    pub duration_ms:   u64,
    pub final_context: Context,
    pub engine:        Arc<EngineServices>,
    pub model:         String,
}

/// Output of the RETRO phase.
#[non_exhaustive]
pub struct Retroed {
    pub graph:       Graph,
    pub outcome:     Result<Outcome, Error>,
    pub run_options: RunOptions,
    pub duration_ms: u64,
    pub services:    Arc<RunServices>,
    pub retro:       Option<Retro>,
}

/// Output of the FINALIZE phase.
#[non_exhaustive]
pub struct Concluded {
    pub outcome:     Result<Outcome, Error>,
    pub conclusion:  Conclusion,
    pub graph:       Graph,
    pub run_options: RunOptions,
    pub services:    Arc<RunServices>,
}

/// Output of the PULL_REQUEST phase.
#[non_exhaustive]
pub struct Finalized {
    pub run_id:        RunId,
    pub outcome:       Result<Outcome, Error>,
    pub conclusion:    Conclusion,
    pub pushed_branch: Option<String>,
    pub pr_url:        Option<String>,
}

/// Options for the TRANSFORM phase.
pub struct TransformOptions {
    pub current_dir:       Option<PathBuf>,
    pub file_resolver:     Option<Arc<dyn FileResolver>>,
    pub inputs:            HashMap<String, toml::Value>,
    pub custom_transforms: Vec<Box<dyn Transform>>,
}

/// Options for the RETRO phase.
pub struct RetroOptions {
    pub run_id:          RunId,
    pub services:        Arc<RunServices>,
    pub workflow_name:   String,
    pub goal:            String,
    pub failed:          bool,
    pub run_duration_ms: u64,
    pub enabled:         bool,
    pub model:           String,
}

/// Options for the FINALIZE phase.
pub struct FinalizeOptions {
    pub run_dir:          PathBuf,
    pub run_id:           RunId,
    pub workflow_name:    String,
    pub preserve_sandbox: bool,
    pub last_git_sha:     Option<String>,
}

/// Options for the PULL_REQUEST phase.
pub struct PullRequestOptions {
    pub pr_config:  Option<PullRequestSettings>,
    pub github_app: Option<fabro_github::GitHubCredentials>,
    pub origin_url: Option<String>,
    pub model:      String,
}