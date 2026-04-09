//! Run domain.
//!
//! `[run]` is the shared execution domain. It may appear in all three config
//! files and layer normally. Subdomains cover model selection, git author,
//! prepare steps, execution posture, checkpoint policy, sandbox selection,
//! notifications, interviews, agent knobs, hooks, SCM targeting, pull-request
//! behavior, and artifact collection.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::duration::Duration;
use super::interp::InterpString;
use super::model_ref::ModelRef;

/// A sparse `[run]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<InterpString>,
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
