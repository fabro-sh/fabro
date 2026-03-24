use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use fabro_agent::Sandbox;
use fabro_graphviz::graph::Graph;
use fabro_validate::Diagnostic;

use crate::checkpoint::Checkpoint;
use crate::conclusion::Conclusion;
use crate::engine::{LifecycleConfig, RunConfig, WorkflowRunEngine};
use crate::error::FabroError;
use crate::event::EventEmitter;
use crate::handler::HandlerRegistry;
use crate::outcome::Outcome;
use fabro_interview::Interviewer;
use fabro_validate::Severity;

/// Output of the PARSE phase.
#[non_exhaustive]
pub struct Parsed {
    pub graph: Graph,
    pub source: String,
}

/// Output of the TRANSFORM phase. Graph is mutable — callers may apply
/// post-transform adjustments (e.g. goal override) before validation.
#[non_exhaustive]
pub struct Transformed {
    pub graph: Graph,
    pub source: String,
}

/// Output of the VALIDATE phase. Always produced (even with errors).
/// Caller inspects diagnostics and decides whether to proceed.
/// Graph is read-only — use accessors, not direct field access.
#[non_exhaustive]
pub struct Validated {
    graph: Graph,
    source: String,
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

    /// Returns `Err(FabroError::Validation)` if any Error-severity diagnostics exist.
    /// Diagnostics remain accessible via `diagnostics()` for printing before this call.
    pub fn raise_on_errors(&self) -> Result<(), FabroError> {
        if self.has_errors() {
            let message = self
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(FabroError::Validation(message));
        }
        Ok(())
    }

    /// Consume into owned graph, source, and diagnostics (used by initialize).
    pub fn into_parts(self) -> (Graph, String, Vec<Diagnostic>) {
        (self.graph, self.source, self.diagnostics)
    }
}

/// Options for the INITIALIZE phase.
pub struct InitOptions {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub dry_run: bool,
    pub emitter: Arc<EventEmitter>,
    pub interviewer: Arc<dyn Interviewer>,
    pub sandbox: Arc<dyn Sandbox>,
    pub registry: HandlerRegistry,
    pub lifecycle: LifecycleConfig,
    pub run_config: RunConfig,
    pub hooks: fabro_hooks::HookConfig,
    pub sandbox_env: HashMap<String, String>,
}

/// Output of the INITIALIZE phase.
#[non_exhaustive]
pub struct Initialized {
    pub graph: Graph,
    pub source: String,
    pub engine: WorkflowRunEngine,
    pub config: RunConfig,
    pub(crate) checkpoint: Option<Checkpoint>,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: Arc<dyn Sandbox>,
}

/// Output of the EXECUTE phase.
#[non_exhaustive]
pub struct Executed {
    pub graph: Graph,
    pub outcome: Result<Outcome, FabroError>,
    pub config: RunConfig,
    pub engine: WorkflowRunEngine,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: Arc<dyn Sandbox>,
    pub duration_ms: u64,
}

/// Output of the RETRO phase.
#[non_exhaustive]
pub struct Retroed {
    pub graph: Graph,
    pub outcome: Result<Outcome, FabroError>,
    pub config: RunConfig,
    pub engine: WorkflowRunEngine,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: Arc<dyn Sandbox>,
    pub duration_ms: u64,
    pub retro: Option<fabro_retro::retro::Retro>,
}

/// Output of the FINALIZE phase.
#[non_exhaustive]
pub struct Finalized {
    pub run_id: String,
    pub outcome: Result<Outcome, FabroError>,
    pub conclusion: Conclusion,
    pub pr_url: Option<String>,
}

/// Options for the TRANSFORM phase.
pub struct TransformOptions {
    pub base_dir: Option<PathBuf>,
    pub custom_transforms: Vec<Box<dyn crate::transform::Transform>>,
}

/// Options for the RETRO phase.
pub struct RetroOptions {
    pub enabled: bool,
    pub dry_run: bool,
    pub llm_client: Option<fabro_llm::client::Client>,
    pub provider: fabro_llm::Provider,
    pub model: String,
}

/// Options for the FINALIZE phase.
pub struct FinalizeOptions {
    pub preserve_sandbox: bool,
    pub pr_config: Option<fabro_config::run::PullRequestConfig>,
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    pub origin_url: Option<String>,
    pub model: String,
    pub last_git_sha: Option<String>,
}
