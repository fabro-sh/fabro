//! Run domain.
//!
//! `[run]` is the shared execution domain. It may appear in all three config
//! files and layer normally. Subdomains cover model selection, git author,
//! prepare steps, execution posture, checkpoint policy, sandbox selection,
//! notifications, interviews, agent knobs, hooks, SCM targeting, pull-request
//! behavior, and artifact collection.

use std::collections::HashMap;
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};

use super::duration::Duration;
use super::interp::InterpString;
use super::model_ref::ModelRef;

/// A structurally resolved `[run]` view for consumers.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSettings {
    pub goal: Option<RunGoal>,
    pub working_dir: Option<InterpString>,
    pub metadata: HashMap<String, String>,
    pub inputs: HashMap<String, toml::Value>,
    pub model: RunModelSettings,
    pub git: RunGitSettings,
    pub prepare: RunPrepareSettings,
    pub execution: RunExecutionSettings,
    pub checkpoint: RunCheckpointSettings,
    pub sandbox: RunSandboxSettings,
    pub notifications: HashMap<String, NotificationRouteSettings>,
    pub interviews: RunInterviewsSettings,
    pub agent: RunAgentSettings,
    pub hooks: Vec<HookDefinition>,
    pub scm: RunScmSettings,
    pub pull_request: Option<PullRequestSettings>,
    pub artifacts: ArtifactsSettings,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            goal: None,
            working_dir: None,
            metadata: HashMap::new(),
            inputs: HashMap::new(),
            model: RunModelSettings::default(),
            git: RunGitSettings::default(),
            prepare: RunPrepareSettings::default(),
            execution: RunExecutionSettings::default(),
            checkpoint: RunCheckpointSettings::default(),
            sandbox: RunSandboxSettings::default(),
            notifications: HashMap::new(),
            interviews: RunInterviewsSettings::default(),
            agent: RunAgentSettings::default(),
            hooks: Vec::new(),
            scm: RunScmSettings::default(),
            pull_request: None,
            artifacts: ArtifactsSettings::default(),
        }
    }
}

/// The resolved source of a run goal.
#[derive(Debug, Clone, PartialEq)]
pub enum RunGoal {
    Inline(InterpString),
    File(InterpString),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunModelSettings {
    pub provider: Option<InterpString>,
    pub name: Option<InterpString>,
    pub fallbacks: Vec<ModelRef>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunGitSettings {
    pub author: Option<GitAuthorSettings>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GitAuthorSettings {
    pub name: Option<InterpString>,
    pub email: Option<InterpString>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunPrepareSettings {
    pub commands: Vec<String>,
    pub timeout_ms: u64,
}

impl Default for RunPrepareSettings {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            timeout_ms: 300_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunExecutionSettings {
    pub mode: RunMode,
    pub approval: ApprovalMode,
    pub retros: bool,
}

impl Default for RunExecutionSettings {
    fn default() -> Self {
        Self {
            mode: RunMode::Normal,
            approval: ApprovalMode::Prompt,
            retros: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunCheckpointSettings {
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunSandboxSettings {
    pub provider: String,
    pub preserve: bool,
    pub devcontainer: bool,
    pub env: HashMap<String, InterpString>,
    pub local: LocalSandboxSettings,
    pub daytona: Option<DaytonaSettings>,
}

impl Default for RunSandboxSettings {
    fn default() -> Self {
        Self {
            provider: "local".to_string(),
            preserve: false,
            devcontainer: false,
            env: HashMap::new(),
            local: LocalSandboxSettings::default(),
            daytona: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalSandboxSettings {
    pub worktree_mode: WorktreeMode,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DaytonaSettings {
    pub auto_stop_interval: Option<i32>,
    pub labels: HashMap<String, String>,
    pub snapshot: Option<DaytonaSnapshotSettings>,
    pub network: Option<DaytonaNetworkLayer>,
    pub skip_clone: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DockerfileSource {
    Inline(String),
    Path { path: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DaytonaSnapshotSettings {
    pub name: String,
    pub cpu: Option<i32>,
    pub memory_gb: Option<i32>,
    pub disk_gb: Option<i32>,
    pub dockerfile: Option<DockerfileSource>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NotificationRouteSettings {
    pub enabled: bool,
    pub provider: Option<String>,
    pub events: Vec<String>,
    pub slack: Option<NotificationProviderSettings>,
    pub discord: Option<NotificationProviderSettings>,
    pub teams: Option<NotificationProviderSettings>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NotificationProviderSettings {
    pub channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunInterviewsSettings {
    pub provider: Option<String>,
    pub slack: Option<InterviewProviderSettings>,
    pub discord: Option<InterviewProviderSettings>,
    pub teams: Option<InterviewProviderSettings>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InterviewProviderSettings {
    pub channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunAgentSettings {
    pub permissions: Option<AgentPermissions>,
    pub mcps: HashMap<String, McpServerSettings>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpServerSettings {
    pub name: String,
    pub transport: McpTransport,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs: u64,
}

impl Default for McpServerSettings {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: McpTransport::Stdio {
                command: Vec::new(),
                env: HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs: 60,
        }
    }
}

impl McpServerSettings {
    #[must_use]
    pub fn startup_timeout(&self) -> StdDuration {
        StdDuration::from_secs(self.startup_timeout_secs)
    }

    #[must_use]
    pub fn tool_timeout(&self) -> StdDuration {
        StdDuration::from_secs(self.tool_timeout_secs)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpTransport {
    Stdio {
        command: Vec<String>,
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        headers: HashMap<String, String>,
    },
    Sandbox {
        command: Vec<String>,
        port: u16,
        env: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    #[default]
    Verify,
    NoVerify,
    Off,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookType {
    Command {
        command: String,
    },
    Http {
        url: String,
        headers: Option<HashMap<String, String>>,
        #[serde(default)]
        allowed_env_vars: Vec<String>,
        #[serde(default)]
        tls: TlsMode,
    },
    Prompt {
        prompt: String,
        model: Option<String>,
    },
    Agent {
        prompt: String,
        model: Option<String>,
        max_tool_rounds: Option<u32>,
    },
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct HookDefinition {
    pub name: Option<String>,
    pub event: HookEvent,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(flatten)]
    pub hook_type: Option<HookType>,
    pub matcher: Option<String>,
    pub blocking: Option<bool>,
    pub timeout_ms: Option<u64>,
    pub sandbox: Option<bool>,
}

impl HookDefinition {
    pub fn resolved_hook_type(&self) -> Option<std::borrow::Cow<'_, HookType>> {
        if let Some(ref hook_type) = self.hook_type {
            return Some(std::borrow::Cow::Borrowed(hook_type));
        }
        self.command.as_ref().map(|command| {
            std::borrow::Cow::Owned(HookType::Command {
                command: command.clone(),
            })
        })
    }

    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.blocking.unwrap_or_else(|| {
            matches!(
                self.event,
                HookEvent::RunStart
                    | HookEvent::StageStart
                    | HookEvent::EdgeSelected
                    | HookEvent::PreToolUse
                    | HookEvent::SandboxReady
            )
        })
    }

    #[must_use]
    pub fn timeout(&self) -> StdDuration {
        if let Some(ms) = self.timeout_ms {
            return StdDuration::from_millis(ms);
        }
        let default_ms = match self.resolved_hook_type().as_deref() {
            Some(HookType::Prompt { .. }) => 30_000,
            _ => 60_000,
        };
        StdDuration::from_millis(default_ms)
    }

    #[must_use]
    pub fn runs_in_sandbox(&self) -> bool {
        self.sandbox.unwrap_or(true)
    }

    #[must_use]
    pub fn effective_name(&self) -> String {
        if let Some(ref name) = self.name {
            return name.clone();
        }
        let event = format!("{:?}", self.event).to_lowercase();
        match self.resolved_hook_type().as_deref() {
            Some(HookType::Command { command }) => {
                let short = &command[..command.floor_char_boundary(20)];
                format!("{event}:{short}")
            }
            Some(HookType::Http { url, .. }) => format!("{event}:{url}"),
            Some(HookType::Prompt { prompt, .. } | HookType::Agent { prompt, .. }) => {
                let short = &prompt[..prompt.floor_char_boundary(20)];
                format!("{event}:{short}")
            }
            None => event,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunScmSettings {
    pub provider: Option<String>,
    pub owner: Option<InterpString>,
    pub repository: Option<InterpString>,
    pub github: Option<ScmGitHubSettings>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScmGitHubSettings;

#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestSettings {
    pub enabled: bool,
    pub draft: bool,
    pub auto_merge: bool,
    pub merge_strategy: MergeStrategy,
}

impl Default for PullRequestSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            draft: true,
            auto_merge: false,
            merge_strategy: MergeStrategy::Squash,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactsSettings {
    pub include: Vec<String>,
}

/// A sparse `[run]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<RunGoalLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<InterpString>,
    /// Flat string-to-string map. Replaces wholesale across layers.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    /// Run inputs: typed scalar values. Replaces wholesale across layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<HashMap<String, toml::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<RunModelLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<RunGitLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare: Option<RunPrepareLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<RunExecutionLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<RunCheckpointLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<RunSandboxLayer>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub notifications: HashMap<String, NotificationRouteLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interviews: Option<InterviewsLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<RunAgentLayer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scm: Option<RunScmLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<RunPullRequestLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<RunArtifactsLayer>,
}

/// The source of a run's goal, either inline literal text or a reference to
/// a file on disk.
///
/// TOML surface:
///
/// ```toml
/// # Inline form
/// [run]
/// goal = "Diagnose and fix CI build failures"
///
/// # File form
/// [run.goal]
/// file = "prompts/fix_build.md"
/// ```
///
/// Relative paths inside the `file` variant are resolved against the
/// directory of the config file that declared them at load time (see
/// `fabro_config::resolve_goal_file_paths`). `${env.NAME}` interpolation is
/// supported inside the `file` path; env-tokenized relative paths stay
/// unresolved until consume time and are then resolved against the run's
/// effective working directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum RunGoalLayer {
    Inline(InterpString),
    File { file: InterpString },
}

/// Outcome of resolving a [`RunGoalLayer`] to its final goal text.
///
/// Carries provenance alongside the text so downstream consumers (e.g. the
/// run manifest builder) can distinguish inline goals from file-sourced
/// goals without having to re-walk the layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRunGoal {
    pub text: String,
    pub source: ResolvedGoalSource,
}

/// Provenance of a [`ResolvedRunGoal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedGoalSource {
    /// Goal text came from a literal `run.goal = "..."` value.
    Inline,
    /// Goal text was read from a file on disk. The absolute path of that
    /// file is carried for provenance / error reporting.
    File { path: std::path::PathBuf },
}

/// `[run.model]` — provider-neutral default model selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunModelLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<InterpString>,
    /// Ordered list of fallback model references. Supports `...` splice marker
    /// at layering time — see [`super::splice_array`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<ModelRefOrSplice>,
}

/// A single `fallbacks` entry: either a parsed `ModelRef` or the splice marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRefOrSplice {
    ModelRef(ModelRef),
    Splice,
}

impl Serialize for ModelRefOrSplice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::ModelRef(m) => m.serialize(serializer),
            Self::Splice => serializer.serialize_str(super::splice_array::SPLICE_MARKER),
        }
    }
}

impl<'de> Deserialize<'de> for ModelRefOrSplice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = String::deserialize(deserializer)?;
        if raw == super::splice_array::SPLICE_MARKER {
            return Ok(Self::Splice);
        }
        let model = raw.parse::<ModelRef>().map_err(D::Error::custom)?;
        Ok(Self::ModelRef(model))
    }
}

/// `[run.git]` — local git behavior such as commit author.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunGitLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<GitAuthorLayer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitAuthorLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<InterpString>,
}

/// `[run.prepare]` — ordered list of preparation steps. Whole list replaces
/// across layers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPrepareLayer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<PrepareStep>,
    /// Optional timeout applied to each prepare step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Duration>,
}

/// A single prepare step. Exactly one of `script` or `command` must be set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<InterpString>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, InterpString>,
}

/// `[run.execution]` — run posture knobs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunExecutionLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RunMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalMode>,
    /// Positive-form: `true` runs retros, `false` skips them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retros: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Normal,
    DryRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Prompt,
    Auto,
}

/// `[run.checkpoint]` — checkpoint policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCheckpointLayer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_globs: Vec<String>,
}

/// `[run.sandbox]` — sandbox selection and execution-environment surface.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSandboxLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devcontainer: Option<bool>,
    /// Sticky merge-by-key across layers.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalSandboxLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daytona: Option<DaytonaSandboxLayer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSandboxLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_mode: Option<WorktreeMode>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeMode {
    Always,
    #[default]
    Clean,
    Dirty,
    Never,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaytonaSandboxLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_stop_interval: Option<i32>,
    /// Sticky merge-by-key (provider-native labels).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<DaytonaSnapshotLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<DaytonaNetworkLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_clone: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaytonaSnapshotLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<super::size::Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<super::size::Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<DaytonaDockerfileLayer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum DaytonaDockerfileLayer {
    Inline(String),
    Path { path: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum DaytonaNetworkLayer {
    Block,
    AllowAll,
    AllowList { allow_list: Vec<String> },
}

/// `[run.notifications.<name>]` — a keyed notification route.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationRouteLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Raw Fabro event names. Splice marker supported at layering time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<StringOrSplice>,
    /// Provider-specific destination subtables. First-pass chat providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<NotificationProviderLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord: Option<NotificationProviderLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teams: Option<NotificationProviderLayer>,
}

/// A single string array entry that may be the splice marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringOrSplice {
    Value(String),
    Splice,
}

impl Serialize for StringOrSplice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(s) => serializer.serialize_str(s),
            Self::Splice => serializer.serialize_str(super::splice_array::SPLICE_MARKER),
        }
    }
}

impl<'de> Deserialize<'de> for StringOrSplice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == super::splice_array::SPLICE_MARKER {
            Ok(Self::Splice)
        } else {
            Ok(Self::Value(s))
        }
    }
}

/// Provider-specific destination fields for a notification route.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationProviderLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<InterpString>,
}

/// `[run.interviews]` — external interview delivery.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterviewsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<InterviewProviderLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord: Option<InterviewProviderLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teams: Option<InterviewProviderLayer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterviewProviderLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<InterpString>,
}

/// `[run.agent]` — agent knobs only (permissions, MCPs).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunAgentLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<AgentPermissions>,
    /// Agent-scoped MCP server entries, keyed by name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mcps: HashMap<String, McpEntryLayer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentPermissions {
    ReadOnly,
    ReadWrite,
    Full,
}

/// A single MCP entry. `type` selects the transport; `script`/`command` are
/// mutually exclusive for process-launching transports. Non-launching HTTP
/// transports use neither field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum McpEntryLayer {
    Http {
        #[serde(default)]
        enabled: Option<bool>,
        url: InterpString,
        #[serde(default)]
        headers: HashMap<String, InterpString>,
        #[serde(default)]
        startup_timeout: Option<Duration>,
        #[serde(default)]
        tool_timeout: Option<Duration>,
    },
    Stdio {
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        script: Option<InterpString>,
        #[serde(default)]
        command: Option<Vec<InterpString>>,
        #[serde(default)]
        env: HashMap<String, InterpString>,
        #[serde(default)]
        startup_timeout: Option<Duration>,
        #[serde(default)]
        tool_timeout: Option<Duration>,
    },
    Sandbox {
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        script: Option<InterpString>,
        #[serde(default)]
        command: Option<Vec<InterpString>>,
        port: u16,
        #[serde(default)]
        env: HashMap<String, InterpString>,
        #[serde(default)]
        startup_timeout: Option<Duration>,
        #[serde(default)]
        tool_timeout: Option<Duration>,
    },
}

/// A run hook entry. Exactly one of `script`, `command`, `url`, `prompt`, or
/// `agent` fields determines the hook behavior. The `id` field, when set, is
/// used for cross-layer replace-by-id merging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookEntry {
    /// Optional merge identity. Hooks with the same `id` replace in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Display-only human name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub event: HookEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<bool>,
    // Exactly one of the following groups is expected:
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<InterpString>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<InterpString>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, InterpString>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_env_vars: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<HookTlsMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<HookAgentMarker>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookTlsMode {
    #[default]
    Verify,
    NoVerify,
    Off,
}

/// Reserved marker for hook entries that use the `agent` hook type. Having
/// this as its own field rather than a flag lets `HookEntry` remain a flat
/// struct without a discriminator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAgentMarker {
    #[default]
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    RunStart,
    RunComplete,
    RunFailed,
    StageStart,
    StageComplete,
    StageFailed,
    StageRetrying,
    EdgeSelected,
    ParallelStart,
    ParallelComplete,
    SandboxReady,
    SandboxCleanup,
    CheckpointSaved,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
}

/// `[run.scm]` — remote SCM host/provider behavior.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScmLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<InterpString>,
    /// Provider-specific SCM leaves. First-pass providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<ScmGitHubLayer>,
}

/// `[run.scm.github]` — GitHub-specific SCM leaf. Intentionally minimal in
/// the first pass; additional branch/checkout context stays on `run` or
/// `run.pull_request` until a concrete use case lands.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScmGitHubLayer;

/// `[run.pull_request]` — provider-neutral PR behavior.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPullRequestLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_strategy: Option<MergeStrategy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeStrategy {
    Squash,
    Merge,
    Rebase,
}

/// `[run.artifacts]` — run artifact collection policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunArtifactsLayer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
}
