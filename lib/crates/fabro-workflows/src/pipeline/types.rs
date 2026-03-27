use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_agent::Sandbox;
use fabro_config::sandbox::WorktreeMode;
use fabro_graphviz::graph::Graph;
use fabro_hooks::HookRunner;
use fabro_interview::Interviewer;
use fabro_llm::Provider;
use fabro_mcp::config::McpServerConfig;
use fabro_model::FallbackTarget;
use fabro_sandbox::daytona::DaytonaConfig;
use fabro_sandbox::docker::DockerSandboxConfig;
#[cfg(feature = "exedev")]
use fabro_sandbox::exe::{ExeConfig, GitCloneParams as ExeGitCloneParams};
use fabro_sandbox::ssh::{GitCloneParams as SshGitCloneParams, SshConfig};
use fabro_validate::Diagnostic;

use crate::context::Context;
use crate::error::FabroError;
use crate::event::EventEmitter;
use crate::outcome::Outcome;
use crate::records::{Checkpoint, Conclusion, RunRecord};
use crate::run_options::{LifecycleOptions, RunOptions};
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

/// Options for the PERSIST phase.
pub struct PersistOptions {
    pub run_dir: PathBuf,
    pub run_record: RunRecord,
}

/// Output of the PERSIST phase. Run directory created, run.json and workflow.fabro written.
#[derive(Debug)]
#[non_exhaustive]
pub struct Persisted {
    graph: Graph,
    source: String,
    diagnostics: Vec<Diagnostic>,
    run_dir: PathBuf,
    run_record: RunRecord,
}

impl Persisted {
    /// Create a new `Persisted` from its parts.
    pub(crate) fn new(
        graph: Graph,
        source: String,
        diagnostics: Vec<Diagnostic>,
        run_dir: PathBuf,
        run_record: RunRecord,
    ) -> Self {
        Self {
            graph,
            source,
            diagnostics,
            run_dir,
            run_record,
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

    pub fn run_record(&self) -> &RunRecord {
        &self.run_record
    }

    /// True if any diagnostic has Error severity.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Returns `Err(FabroError::Validation)` if any Error-severity diagnostics exist.
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

    /// Consume into owned graph, source, diagnostics, run dir, and run record.
    pub fn into_parts(self) -> (Graph, String, Vec<Diagnostic>, PathBuf, RunRecord) {
        (
            self.graph,
            self.source,
            self.diagnostics,
            self.run_dir,
            self.run_record,
        )
    }

    /// Load a previously persisted run from disk.
    pub fn load(run_dir: &Path) -> Result<Self, FabroError> {
        super::persist::load(run_dir)
    }
}

/// Options for the INITIALIZE phase.
pub enum SandboxSpec {
    Local {
        working_directory: PathBuf,
    },
    Docker {
        config: DockerSandboxConfig,
    },
    Daytona {
        config: DaytonaConfig,
        github_app: Option<fabro_github::GitHubAppCredentials>,
        run_id: Option<String>,
        clone_branch: Option<String>,
    },
    #[cfg(feature = "exedev")]
    Exe {
        config: ExeConfig,
        clone_params: Option<ExeGitCloneParams>,
        run_id: Option<String>,
        github_app: Option<fabro_github::GitHubAppCredentials>,
        mgmt_destination: String,
    },
    Ssh {
        config: SshConfig,
        clone_params: Option<SshGitCloneParams>,
        run_id: Option<String>,
        github_app: Option<fabro_github::GitHubAppCredentials>,
    },
}

#[derive(Clone)]
pub struct LlmSpec {
    pub model: String,
    pub provider: Provider,
    pub fallback_chain: Vec<FallbackTarget>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub dry_run: bool,
}

#[derive(Clone)]
pub struct SandboxEnvSpec {
    pub devcontainer_env: HashMap<String, String>,
    pub toml_env: HashMap<String, String>,
    pub github_permissions: Option<HashMap<String, String>>,
    pub origin_url: Option<String>,
}

#[derive(Clone)]
pub struct DevcontainerSpec {
    pub enabled: bool,
    pub resolve_dir: PathBuf,
}

pub struct InitOptions {
    pub run_id: String,
    pub dry_run: bool,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: SandboxSpec,
    pub llm: LlmSpec,
    pub interviewer: Arc<dyn Interviewer>,
    pub lifecycle: LifecycleOptions,
    pub run_options: RunOptions,
    pub hooks: fabro_hooks::HookConfig,
    pub sandbox_env: SandboxEnvSpec,
    pub devcontainer: Option<DevcontainerSpec>,
    pub git: Option<crate::run_options::GitCheckpointOptions>,
    pub worktree_mode: Option<WorktreeMode>,
    pub registry_override: Option<Arc<crate::handler::HandlerRegistry>>,
    pub checkpoint: Option<Checkpoint>,
    pub seed_context: Option<Context>,
}

/// Output of the INITIALIZE phase.
#[non_exhaustive]
pub struct Initialized {
    pub graph: Graph,
    pub source: String,
    pub run_options: RunOptions,
    pub(crate) checkpoint: Option<Checkpoint>,
    pub(crate) seed_context: Option<Context>,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: Arc<dyn Sandbox>,
    pub registry: Arc<crate::handler::HandlerRegistry>,
    pub hook_runner: Option<Arc<HookRunner>>,
    pub env: HashMap<String, String>,
    pub dry_run: bool,
    pub llm_client: Option<fabro_llm::client::Client>,
    pub model: String,
    pub provider: Provider,
}

/// Output of the EXECUTE phase.
#[non_exhaustive]
pub struct Executed {
    pub graph: Graph,
    pub outcome: Result<Outcome, FabroError>,
    pub run_options: RunOptions,
    pub hook_runner: Option<Arc<HookRunner>>,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: Arc<dyn Sandbox>,
    pub duration_ms: u64,
    pub final_context: Context,
    pub llm_client: Option<fabro_llm::client::Client>,
    pub model: String,
    pub provider: Provider,
}

/// Output of the RETRO phase.
#[non_exhaustive]
pub struct Retroed {
    pub graph: Graph,
    pub outcome: Result<Outcome, FabroError>,
    pub run_options: RunOptions,
    pub hook_runner: Option<Arc<HookRunner>>,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: Arc<dyn Sandbox>,
    pub duration_ms: u64,
    pub retro: Option<fabro_retro::retro::Retro>,
}

/// Output of the FINALIZE phase.
#[non_exhaustive]
pub struct Concluded {
    pub run_id: String,
    pub outcome: Result<Outcome, FabroError>,
    pub conclusion: Conclusion,
    pub pushed_branch: Option<String>,
    pub graph: Graph,
    pub run_options: RunOptions,
    pub emitter: Arc<EventEmitter>,
}

/// Output of the PULL_REQUEST phase.
#[non_exhaustive]
pub struct Finalized {
    pub run_id: String,
    pub outcome: Result<Outcome, FabroError>,
    pub conclusion: Conclusion,
    pub pushed_branch: Option<String>,
    pub pr_url: Option<String>,
}

/// Options for the TRANSFORM phase.
pub struct TransformOptions {
    pub base_dir: Option<PathBuf>,
    pub custom_transforms: Vec<Box<dyn crate::transforms::Transform>>,
}

/// Options for the RETRO phase.
pub struct RetroOptions {
    pub run_id: String,
    pub workflow_name: String,
    pub goal: String,
    pub run_dir: PathBuf,
    pub sandbox: Arc<dyn Sandbox>,
    pub emitter: Option<Arc<EventEmitter>>,
    pub failed: bool,
    pub run_duration_ms: u64,
    pub enabled: bool,
    pub llm_client: Option<fabro_llm::client::Client>,
    pub provider: Provider,
    pub model: String,
}

/// Options for the FINALIZE phase.
pub struct FinalizeOptions {
    pub run_dir: PathBuf,
    pub run_id: String,
    pub workflow_name: String,
    pub hook_runner: Option<Arc<HookRunner>>,
    pub preserve_sandbox: bool,
    pub last_git_sha: Option<String>,
}

/// Options for the PULL_REQUEST phase.
pub struct PullRequestOptions {
    pub run_dir: PathBuf,
    pub pr_config: Option<fabro_config::run::PullRequestSettings>,
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    pub origin_url: Option<String>,
    pub model: String,
}
