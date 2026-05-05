//! Run domain.
//!
//! `[run]` is the shared execution domain. It may appear in all three config
//! files and layer normally. Subdomains cover model selection, git author,
//! prepare steps, execution posture, checkpoint policy, sandbox selection,
//! notifications, interviews, agent knobs, hooks, SCM targeting, pull-request
//! behavior, and artifact collection.

use std::collections::HashMap;
use std::time::Duration as StdDuration;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

use super::interp::InterpString;
use super::model_ref::ModelRef;

/// A structurally resolved `[run]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunNamespace {
    pub goal:          Option<RunGoal>,
    pub working_dir:   Option<InterpString>,
    pub metadata:      HashMap<String, String>,
    pub inputs:        HashMap<String, toml::Value>,
    pub model:         RunModelSettings,
    pub git:           RunGitSettings,
    pub prepare:       RunPrepareSettings,
    pub execution:     RunExecutionSettings,
    pub checkpoint:    RunCheckpointSettings,
    pub sandbox:       RunSandboxSettings,
    pub notifications: HashMap<String, NotificationRouteSettings>,
    pub interviews:    RunInterviewsSettings,
    pub agent:         RunAgentSettings,
    pub hooks:         Vec<HookDefinition>,
    pub scm:           RunScmSettings,
    pub pull_request:  Option<PullRequestSettings>,
    pub artifacts:     ArtifactsSettings,
    pub integrations:  RunIntegrationsSettings,
}

/// `[run.integrations]` — run-level integration knobs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunIntegrationsSettings {
    pub github: RunIntegrationsGithubSettings,
}

/// `[run.integrations.github]` — runtime GitHub token shape.
///
/// `permissions` is empty when no token should be requested. The
/// presence-vs-clear distinction is only meaningful at the layer-merge
/// stage; the resolved form collapses both `None` and `Some({})` into an
/// empty map.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunIntegrationsGithubSettings {
    pub permissions: HashMap<String, InterpString>,
}

impl RunIntegrationsGithubSettings {
    /// Whether the run config asks Fabro to mint a `GITHUB_TOKEN` for the
    /// sandbox. Empty `permissions` means "no token requested" — the only
    /// resolved-form sentinel for this state.
    pub fn is_token_requested(&self) -> bool {
        !self.permissions.is_empty()
    }

    /// Resolve every `permissions` value's `{{ env.* }}` tokens via
    /// `lookup`, falling back to `InterpString::as_source()` when
    /// resolution fails so callers see a recognizable diagnostic instead
    /// of a silently dropped key. The `lookup` seam keeps tests free of
    /// process-env coupling; production callers pass a thin wrapper over
    /// `std::env::var`.
    pub fn resolve_permissions<F>(&self, mut lookup: F) -> HashMap<String, String>
    where
        F: FnMut(&str) -> Option<String>,
    {
        self.permissions
            .iter()
            .map(|(name, value)| {
                let resolved = value
                    .resolve(&mut lookup)
                    .map_or_else(|_| value.as_source(), |resolved| resolved.value);
                (name.clone(), resolved)
            })
            .collect()
    }
}

#[cfg(test)]
mod run_integrations_github_tests {
    use super::{HashMap, InterpString, RunIntegrationsGithubSettings};

    fn settings(permissions: &[(&str, &str)]) -> RunIntegrationsGithubSettings {
        RunIntegrationsGithubSettings {
            permissions: permissions
                .iter()
                .map(|(k, v)| ((*k).to_string(), InterpString::parse(v)))
                .collect(),
        }
    }

    #[test]
    fn is_token_requested_reflects_permissions_presence() {
        assert!(!settings(&[]).is_token_requested());
        assert!(settings(&[("issues", "read")]).is_token_requested());
    }

    #[test]
    fn resolve_permissions_substitutes_env_tokens_via_lookup() {
        let s = settings(&[("issues", "{{ env.GH_PERM_LEVEL }}"), ("contents", "read")]);
        let resolved = s.resolve_permissions(|name| match name {
            "GH_PERM_LEVEL" => Some("write".to_string()),
            _ => None,
        });
        assert_eq!(resolved.get("issues"), Some(&"write".to_string()));
        assert_eq!(resolved.get("contents"), Some(&"read".to_string()));
    }

    #[test]
    fn resolve_permissions_falls_back_to_source_when_lookup_fails() {
        let s = settings(&[("issues", "{{ env.GH_PERM_MISSING }}")]);
        let resolved = s.resolve_permissions(|_| None);
        assert_eq!(
            resolved.get("issues"),
            Some(&"{{ env.GH_PERM_MISSING }}".to_string())
        );
    }

    #[test]
    fn resolve_permissions_is_empty_for_empty_settings() {
        let s: HashMap<String, String> = settings(&[]).resolve_permissions(|_| None);
        assert!(s.is_empty());
    }
}

/// The resolved source of a run goal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RunGoal {
    Inline(InterpString),
    File(InterpString),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunModelSettings {
    pub provider:  Option<InterpString>,
    pub name:      Option<InterpString>,
    pub fallbacks: Vec<ModelRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunGitSettings {
    pub author: Option<GitAuthorSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GitAuthorSettings {
    pub name:  Option<InterpString>,
    pub email: Option<InterpString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunPrepareSettings {
    pub commands:   Vec<String>,
    pub timeout_ms: u64,
}

impl Default for RunPrepareSettings {
    fn default() -> Self {
        Self {
            commands:   Vec::new(),
            timeout_ms: 300_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunExecutionSettings {
    pub mode:     RunMode,
    pub approval: ApprovalMode,
    pub retros:   bool,
}

impl Default for RunExecutionSettings {
    fn default() -> Self {
        Self {
            mode:     RunMode::Normal,
            approval: ApprovalMode::Prompt,
            retros:   true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunCheckpointSettings {
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSandboxSettings {
    pub provider:     String,
    pub preserve:     bool,
    pub devcontainer: bool,
    pub env:          HashMap<String, InterpString>,
    pub local:        LocalSandboxSettings,
    pub docker:       Option<DockerSettings>,
    pub daytona:      Option<DaytonaSettings>,
}

impl Default for RunSandboxSettings {
    fn default() -> Self {
        Self {
            provider:     "local".to_string(),
            preserve:     false,
            devcontainer: false,
            env:          HashMap::new(),
            local:        LocalSandboxSettings::default(),
            docker:       None,
            daytona:      None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalSandboxSettings {
    pub worktree_mode: WorktreeMode,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DockerSettings {
    pub image:        String,
    pub network_mode: Option<String>,
    pub memory_limit: Option<i64>,
    pub cpu_quota:    Option<i64>,
    pub env_vars:     HashMap<String, InterpString>,
    pub skip_clone:   bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DaytonaSettings {
    pub auto_stop_interval: Option<i32>,
    pub labels:             HashMap<String, String>,
    pub snapshot:           Option<DaytonaSnapshotSettings>,
    pub network:            Option<DaytonaNetworkLayer>,
    pub skip_clone:         bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DockerfileSource {
    Inline(String),
    Path { path: String },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DockerfileSourceRepr {
    Inline { value: String },
    Path { path: String },
}

impl Serialize for DockerfileSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("DockerfileSource", 2)?;
        match self {
            Self::Inline(value) => {
                state.serialize_field("type", "inline")?;
                state.serialize_field("value", value)?;
            }
            Self::Path { path } => {
                state.serialize_field("type", "path")?;
                state.serialize_field("path", path)?;
            }
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for DockerfileSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match DockerfileSourceRepr::deserialize(deserializer)? {
            DockerfileSourceRepr::Inline { value } => Ok(Self::Inline(value)),
            DockerfileSourceRepr::Path { path } => Ok(Self::Path { path }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaytonaSnapshotSettings {
    pub name:       String,
    pub cpu:        Option<i32>,
    pub memory_gb:  Option<i32>,
    pub disk_gb:    Option<i32>,
    pub dockerfile: Option<DockerfileSource>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationRouteSettings {
    pub enabled:  bool,
    pub provider: Option<String>,
    pub events:   Vec<String>,
    pub slack:    Option<NotificationProviderSettings>,
    pub discord:  Option<NotificationProviderSettings>,
    pub teams:    Option<NotificationProviderSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationProviderSettings {
    pub channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInterviewsSettings {
    pub provider: Option<String>,
    pub slack:    Option<InterviewProviderSettings>,
    pub discord:  Option<InterviewProviderSettings>,
    pub teams:    Option<InterviewProviderSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InterviewProviderSettings {
    pub channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunAgentSettings {
    pub permissions: Option<AgentPermissions>,
    pub mcps:        HashMap<String, McpServerSettings>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerSettings {
    pub name:                 String,
    pub transport:            McpTransport,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs:    u64,
}

impl Default for McpServerSettings {
    fn default() -> Self {
        Self {
            name:                 String::new(),
            transport:            McpTransport::Stdio {
                command: Vec::new(),
                env:     HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs:    60,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: Vec<String>,
        env:     HashMap<String, String>,
    },
    Http {
        url:     String,
        headers: HashMap<String, String>,
    },
    Sandbox {
        command: Vec<String>,
        port:    u16,
        env:     HashMap<String, String>,
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
        url:              String,
        headers:          Option<HashMap<String, String>>,
        #[serde(default)]
        allowed_env_vars: Vec<String>,
        #[serde(default)]
        tls:              TlsMode,
    },
    Prompt {
        prompt: String,
        model:  Option<String>,
    },
    Agent {
        prompt:          String,
        model:           Option<String>,
        max_tool_rounds: Option<u32>,
    },
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct HookDefinition {
    pub name:       Option<String>,
    pub event:      HookEvent,
    #[serde(default)]
    pub command:    Option<String>,
    #[serde(flatten)]
    pub hook_type:  Option<HookType>,
    pub matcher:    Option<String>,
    pub blocking:   Option<bool>,
    pub timeout_ms: Option<u64>,
    pub sandbox:    Option<bool>,
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
        self.blocking.unwrap_or({
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunScmSettings {
    pub provider:   Option<String>,
    pub owner:      Option<InterpString>,
    pub repository: Option<InterpString>,
    pub github:     Option<ScmGitHubSettings>,
}

#[expect(
    clippy::empty_structs_with_brackets,
    reason = "resolved empty table must stay object-shaped on the wire"
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScmGitHubSettings {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequestSettings {
    pub enabled:        bool,
    pub draft:          bool,
    pub auto_merge:     bool,
    pub merge_strategy: MergeStrategy,
}

impl Default for PullRequestSettings {
    fn default() -> Self {
        Self {
            enabled:        false,
            draft:          true,
            auto_merge:     false,
            merge_strategy: MergeStrategy::Squash,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArtifactsSettings {
    pub include: Vec<String>,
}
/// Outcome of resolving a [`RunGoal`] to its final goal text.
///
/// Carries provenance alongside the text so downstream consumers (e.g. the
/// run manifest builder) can distinguish inline goals from file-sourced goals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRunGoal {
    pub text:   String,
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

/// A single prepare step. Exactly one of `script` or `command` must be set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script:  Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<InterpString>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env:     HashMap<String, InterpString>,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeMode {
    Always,
    #[default]
    Clean,
    Dirty,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum DaytonaNetworkLayer {
    Block,
    AllowAll,
    AllowList { allow_list: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentPermissions {
    ReadOnly,
    ReadWrite,
    Full,
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

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}
