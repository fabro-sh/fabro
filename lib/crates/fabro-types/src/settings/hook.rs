use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// Lifecycle events that can trigger user-defined hooks.
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
    /// Reserved: hooks for this event are not yet invoked by the engine.
    SandboxReady,
    /// Reserved: hooks for this event are not yet invoked by the engine.
    SandboxCleanup,
    CheckpointSaved,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
}

impl HookEvent {
    /// Whether hooks for this event block execution by default.
    #[must_use]
    pub fn is_blocking_by_default(self) -> bool {
        matches!(
            self,
            Self::RunStart
                | Self::StageStart
                | Self::EdgeSelected
                | Self::PreToolUse
                | Self::SandboxReady
        )
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RunStart => "run_start",
            Self::RunComplete => "run_complete",
            Self::RunFailed => "run_failed",
            Self::StageStart => "stage_start",
            Self::StageComplete => "stage_complete",
            Self::StageFailed => "stage_failed",
            Self::StageRetrying => "stage_retrying",
            Self::EdgeSelected => "edge_selected",
            Self::ParallelStart => "parallel_start",
            Self::ParallelComplete => "parallel_complete",
            Self::SandboxReady => "sandbox_ready",
            Self::SandboxCleanup => "sandbox_cleanup",
            Self::CheckpointSaved => "checkpoint_saved",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::PostToolUseFailure => "post_tool_use_failure",
        })
    }
}

/// TLS verification mode for HTTP hooks.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// Require `https://` and verify certificates (default).
    #[default]
    Verify,
    /// Require `https://` but skip certificate verification.
    NoVerify,
    /// Allow `http://`; skip certificate verification for `https://`.
    Off,
}

/// How a hook is executed.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookType {
    Command {
        command: String,
    },
    Http {
        url: String,
        headers: Option<std::collections::HashMap<String, String>>,
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

/// A single hook definition.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct HookDefinition {
    pub name: Option<String>,
    pub event: HookEvent,
    /// Inline command shorthand — if set, implies `type = "command"`.
    #[serde(default)]
    pub command: Option<String>,
    /// Explicit hook type (command or http). If omitted and `command` is set,
    /// defaults to `Command`.
    #[serde(flatten)]
    pub hook_type: Option<HookType>,
    /// Regex matched against node_id, handler_type, or event-specific fields.
    pub matcher: Option<String>,
    /// Override the event's default blocking behavior.
    pub blocking: Option<bool>,
    /// Timeout in milliseconds (default: 60_000).
    pub timeout_ms: Option<u64>,
    /// Run inside the sandbox (true, default) or on the host (false).
    pub sandbox: Option<bool>,
}

impl HookDefinition {
    /// Resolve the effective hook type: explicit `hook_type` wins, then `command`
    /// shorthand, then error.
    pub fn resolved_hook_type(&self) -> Option<Cow<'_, HookType>> {
        if let Some(ref ht) = self.hook_type {
            return Some(Cow::Borrowed(ht));
        }
        self.command.as_ref().map(|cmd| {
            Cow::Owned(HookType::Command {
                command: cmd.clone(),
            })
        })
    }

    /// Whether this hook is blocking for its event.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.blocking
            .unwrap_or_else(|| self.event.is_blocking_by_default())
    }

    /// Timeout duration for this hook.
    ///
    /// Defaults: 30s for prompt hooks, 60s for all others.
    #[must_use]
    pub fn timeout(&self) -> std::time::Duration {
        if let Some(ms) = self.timeout_ms {
            return std::time::Duration::from_millis(ms);
        }
        let default_ms = match self.resolved_hook_type().as_deref() {
            Some(HookType::Prompt { .. }) => 30_000,
            _ => 60_000,
        };
        std::time::Duration::from_millis(default_ms)
    }

    /// Whether this hook runs in the sandbox.
    #[must_use]
    pub fn runs_in_sandbox(&self) -> bool {
        self.sandbox.unwrap_or(true)
    }

    /// The effective name: explicit name or a generated one.
    #[must_use]
    pub fn effective_name(&self) -> String {
        if let Some(ref n) = self.name {
            return n.clone();
        }
        let event_str = self.event.to_string();
        match self.resolved_hook_type().as_deref() {
            Some(HookType::Command { ref command }) => {
                let short = &command[..command.floor_char_boundary(20)];
                format!("{event_str}:{short}")
            }
            Some(HookType::Http { ref url, .. }) => format!("{event_str}:{url}"),
            Some(HookType::Prompt { ref prompt, .. } | HookType::Agent { ref prompt, .. }) => {
                let short = &prompt[..prompt.floor_char_boundary(20)];
                format!("{event_str}:{short}")
            }
            None => event_str,
        }
    }
}

/// Top-level hook configuration: a list of hook definitions.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct HookSettings {
    #[serde(default)]
    pub hooks: Vec<HookDefinition>,
}

impl HookSettings {
    /// Merge with another config. Concatenates lists; on name collisions, `other` wins.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        let mut by_name: std::collections::HashMap<String, HookDefinition> =
            std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for hook in self.hooks {
            let name = hook.effective_name();
            if !by_name.contains_key(&name) {
                order.push(name.clone());
            }
            by_name.insert(name, hook);
        }
        for hook in other.hooks {
            let name = hook.effective_name();
            if !by_name.contains_key(&name) {
                order.push(name.clone());
            }
            by_name.insert(name, hook);
        }

        let hooks = order
            .into_iter()
            .filter_map(|name| by_name.remove(&name))
            .collect();

        Self { hooks }
    }
}
