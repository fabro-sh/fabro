//! Hook configuration runtime types.
//!
//! These types are the runtime shape that the hook executor consumes. The
//! v2 parse tree under `fabro_types::settings::v2::run::HookEntry` is the
//! *config-file* shape; this module lives in `fabro-hooks` because the
//! behavior methods (`is_blocking`, `timeout`, `resolved_hook_type`,
//! `runs_in_sandbox`, `effective_name`) are runtime concerns owned by the
//! executor.
//!
//! [`bridge_hook`] converts a v2 `HookEntry` into the runtime
//! [`HookDefinition`] and lives here (not in `fabro-types`) so the runtime
//! shape stays owned by this crate.

use std::borrow::Cow;

use fabro_types::settings::v2::InterpString;
use fabro_types::settings::v2::run::{
    HookAgentMarker, HookEntry, HookEvent as V2HookEvent, HookTlsMode as V2HookTlsMode,
};
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

/// Convert a v2 [`HookEntry`] into the runtime [`HookDefinition`] shape
/// this crate's executor consumes.
#[must_use]
pub fn bridge_hook(hook: &HookEntry) -> HookDefinition {
    let hook_type = resolve_hook_type(hook);
    // If the hook is a script/command form, emit via the shorthand so
    // HookDefinition.command holds the full command and
    // HookDefinition.hook_type stays None. This avoids the duplicate
    // `command` key that would otherwise appear under `#[serde(flatten)]`.
    let command = if let Some(script) = &hook.script {
        Some(interp_to_string(script))
    } else {
        hook.command.as_ref().map(|command| {
            command
                .iter()
                .map(interp_to_string)
                .collect::<Vec<_>>()
                .join(" ")
        })
    };
    HookDefinition {
        name: hook.name.clone().or_else(|| hook.id.clone()),
        event: bridge_hook_event(hook.event),
        command,
        hook_type,
        matcher: hook.matcher.clone(),
        blocking: hook.blocking,
        timeout_ms: hook
            .timeout
            .map(|d| u64::try_from(d.as_std().as_millis()).unwrap_or(u64::MAX)),
        sandbox: hook.sandbox,
    }
}

fn resolve_hook_type(hook: &HookEntry) -> Option<HookType> {
    if hook.script.is_some() || hook.command.is_some() {
        return None;
    }
    if let Some(url) = &hook.url {
        let headers = if hook.headers.is_empty() {
            None
        } else {
            Some(
                hook.headers
                    .iter()
                    .map(|(k, v)| (k.clone(), interp_to_string(v)))
                    .collect(),
            )
        };
        let tls = match hook.tls {
            Some(V2HookTlsMode::Verify) => TlsMode::Verify,
            Some(V2HookTlsMode::NoVerify) => TlsMode::NoVerify,
            Some(V2HookTlsMode::Off) => TlsMode::Off,
            None => TlsMode::default(),
        };
        return Some(HookType::Http {
            url: interp_to_string(url),
            headers,
            allowed_env_vars: hook.allowed_env_vars.clone(),
            tls,
        });
    }
    if matches!(hook.agent, Some(HookAgentMarker::Enabled)) {
        return Some(HookType::Agent {
            prompt: hook
                .prompt
                .as_ref()
                .map(interp_to_string)
                .unwrap_or_default(),
            model: hook.model.as_ref().map(interp_to_string),
            max_tool_rounds: hook.max_tool_rounds,
        });
    }
    hook.prompt.as_ref().map(|prompt| HookType::Prompt {
        prompt: interp_to_string(prompt),
        model: hook.model.as_ref().map(interp_to_string),
    })
}

fn bridge_hook_event(event: V2HookEvent) -> HookEvent {
    match event {
        V2HookEvent::RunStart => HookEvent::RunStart,
        V2HookEvent::RunComplete => HookEvent::RunComplete,
        V2HookEvent::RunFailed => HookEvent::RunFailed,
        V2HookEvent::StageStart => HookEvent::StageStart,
        V2HookEvent::StageComplete => HookEvent::StageComplete,
        V2HookEvent::StageFailed => HookEvent::StageFailed,
        V2HookEvent::StageRetrying => HookEvent::StageRetrying,
        V2HookEvent::EdgeSelected => HookEvent::EdgeSelected,
        V2HookEvent::ParallelStart => HookEvent::ParallelStart,
        V2HookEvent::ParallelComplete => HookEvent::ParallelComplete,
        V2HookEvent::SandboxReady => HookEvent::SandboxReady,
        V2HookEvent::SandboxCleanup => HookEvent::SandboxCleanup,
        V2HookEvent::CheckpointSaved => HookEvent::CheckpointSaved,
        V2HookEvent::PreToolUse => HookEvent::PreToolUse,
        V2HookEvent::PostToolUse => HookEvent::PostToolUse,
        V2HookEvent::PostToolUseFailure => HookEvent::PostToolUseFailure,
    }
}

fn interp_to_string(value: &InterpString) -> String {
    value.as_source()
}
