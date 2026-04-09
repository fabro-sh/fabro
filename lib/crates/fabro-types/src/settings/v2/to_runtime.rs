//! v2 → runtime-type conversion helpers.
//!
//! The runtime types in `fabro_types::settings::{hook,mcp,run,sandbox}` are
//! the shapes that downstream crates (fabro-workflow, fabro-mcp,
//! fabro-sandbox, fabro-hooks) still consume at runtime. Each helper here
//! reads the v2 parse tree and builds the equivalent runtime value.
//!
//! These helpers replace the deleted `bridge_to_old` seam from Stage 6.2.
//! They are narrower: each builds a single runtime type from a single v2
//! subtree, rather than assembling a full legacy [`Settings`] struct.
//!
//! Stage 6.3 deletes the legacy runtime types themselves. At that point
//! these helpers either disappear or get rewritten against the v2-native
//! replacements.

use std::collections::HashMap;

use super::interp::InterpString;
use super::run::{
    HookEntry as V2HookEntry, HookEvent as V2HookEvent, McpEntryLayer,
    MergeStrategy as V2MergeStrategy, RunArtifactsLayer, RunPullRequestLayer, RunSandboxLayer,
    WorktreeMode as V2WorktreeMode,
};
use crate::settings::hook::{
    HookDefinition, HookEvent as OldHookEvent, HookType as OldHookType, TlsMode as OldTlsMode,
};
use crate::settings::mcp::{McpServerEntry, McpTransport};
use crate::settings::run::{
    ArtifactsSettings, MergeStrategy as OldMergeStrategy, PullRequestSettings,
};
use crate::settings::sandbox::{
    DaytonaNetwork, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    LocalSandboxSettings, SandboxSettings, WorktreeMode as OldWorktreeMode,
};

pub fn bridge_sandbox(sb: &RunSandboxLayer) -> SandboxSettings {
    SandboxSettings {
        provider: sb.provider.clone(),
        preserve: sb.preserve,
        devcontainer: sb.devcontainer,
        local: sb.local.as_ref().map(|local| LocalSandboxSettings {
            worktree_mode: local
                .worktree_mode
                .map(bridge_worktree_mode)
                .unwrap_or_default(),
        }),
        daytona: sb.daytona.as_ref().map(|d| DaytonaSettings {
            auto_stop_interval: d.auto_stop_interval,
            labels: if d.labels.is_empty() {
                None
            } else {
                Some(d.labels.clone())
            },
            snapshot: d.snapshot.as_ref().and_then(|s| {
                s.name.as_ref().map(|name| DaytonaSnapshotSettings {
                    name: name.clone(),
                    cpu: s.cpu,
                    memory: s.memory.map(|sz| size_to_gb_i32(sz.as_bytes())),
                    disk: s.disk.map(|sz| size_to_gb_i32(sz.as_bytes())),
                    dockerfile: s.dockerfile.as_ref().map(|d| match d {
                        super::run::DaytonaDockerfileLayer::Inline(text) => {
                            DockerfileSource::Inline(text.clone())
                        }
                        super::run::DaytonaDockerfileLayer::Path { path } => {
                            DockerfileSource::Path { path: path.clone() }
                        }
                    }),
                })
            }),
            network: d.network.as_ref().map(|n| match n {
                super::run::DaytonaNetworkLayer::Block => DaytonaNetwork::Block,
                super::run::DaytonaNetworkLayer::AllowAll => DaytonaNetwork::AllowAll,
                super::run::DaytonaNetworkLayer::AllowList { allow_list } => {
                    DaytonaNetwork::AllowList(allow_list.clone())
                }
            }),
            skip_clone: d.skip_clone.unwrap_or(false),
        }),
        env: if sb.env.is_empty() {
            None
        } else {
            Some(
                sb.env
                    .iter()
                    .map(|(k, v)| (k.clone(), interp_to_string(v)))
                    .collect(),
            )
        },
    }
}

pub fn bridge_worktree_mode(m: V2WorktreeMode) -> OldWorktreeMode {
    match m {
        V2WorktreeMode::Always => OldWorktreeMode::Always,
        V2WorktreeMode::Clean => OldWorktreeMode::Clean,
        V2WorktreeMode::Dirty => OldWorktreeMode::Dirty,
        V2WorktreeMode::Never => OldWorktreeMode::Never,
    }
}

pub fn bridge_merge_strategy(m: V2MergeStrategy) -> OldMergeStrategy {
    match m {
        V2MergeStrategy::Squash => OldMergeStrategy::Squash,
        V2MergeStrategy::Merge => OldMergeStrategy::Merge,
        V2MergeStrategy::Rebase => OldMergeStrategy::Rebase,
    }
}

pub fn bridge_pull_request(pr: &RunPullRequestLayer) -> PullRequestSettings {
    PullRequestSettings {
        enabled: pr.enabled.unwrap_or(false),
        draft: pr.draft.unwrap_or(true),
        auto_merge: pr.auto_merge.unwrap_or(false),
        merge_strategy: pr
            .merge_strategy
            .map(bridge_merge_strategy)
            .unwrap_or_default(),
    }
}

pub fn bridge_run_artifacts(artifacts: &RunArtifactsLayer) -> ArtifactsSettings {
    ArtifactsSettings {
        include: artifacts.include.clone(),
    }
}

pub fn bridge_mcps(mcps: &HashMap<String, McpEntryLayer>) -> HashMap<String, McpServerEntry> {
    mcps.iter()
        .map(|(name, entry)| (name.clone(), bridge_mcp_entry(entry)))
        .collect()
}

pub fn bridge_mcp_entry(entry: &McpEntryLayer) -> McpServerEntry {
    let transport = match entry {
        McpEntryLayer::Stdio {
            script,
            command,
            env,
            ..
        } => {
            let command_vec: Vec<String> = if let Some(script) = script {
                vec!["sh".into(), "-c".into(), interp_to_string(script)]
            } else if let Some(command) = command {
                command.iter().map(interp_to_string).collect()
            } else {
                Vec::new()
            };
            McpTransport::Stdio {
                command: command_vec,
                env: env
                    .iter()
                    .map(|(k, v)| (k.clone(), interp_to_string(v)))
                    .collect(),
            }
        }
        McpEntryLayer::Http { url, headers, .. } => McpTransport::Http {
            url: interp_to_string(url),
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), interp_to_string(v)))
                .collect(),
        },
        McpEntryLayer::Sandbox {
            script,
            command,
            port,
            env,
            ..
        } => {
            let command_vec: Vec<String> = if let Some(script) = script {
                vec!["sh".into(), "-c".into(), interp_to_string(script)]
            } else if let Some(command) = command {
                command.iter().map(interp_to_string).collect()
            } else {
                Vec::new()
            };
            McpTransport::Sandbox {
                command: command_vec,
                port: *port,
                env: env
                    .iter()
                    .map(|(k, v)| (k.clone(), interp_to_string(v)))
                    .collect(),
            }
        }
    };

    let (startup_secs, tool_secs) = match entry {
        McpEntryLayer::Http {
            startup_timeout,
            tool_timeout,
            ..
        }
        | McpEntryLayer::Stdio {
            startup_timeout,
            tool_timeout,
            ..
        }
        | McpEntryLayer::Sandbox {
            startup_timeout,
            tool_timeout,
            ..
        } => (
            startup_timeout.map_or(10, |d| d.as_std().as_secs()),
            tool_timeout.map_or(60, |d| d.as_std().as_secs()),
        ),
    };

    McpServerEntry {
        transport,
        startup_timeout_secs: startup_secs,
        tool_timeout_secs: tool_secs,
    }
}

pub fn bridge_hook(hook: &V2HookEntry) -> HookDefinition {
    let hook_type = resolve_hook_type(hook);
    // If the hook is a script/command form, emit via the shorthand so the
    // old HookDefinition.command field holds the full command and
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

fn resolve_hook_type(hook: &V2HookEntry) -> Option<OldHookType> {
    // Script/command-shorthand hooks are emitted via the top-level
    // HookDefinition.command field in bridge_hook, not here, to avoid
    // the `#[serde(flatten)]` duplicate-field collision between the
    // outer HookDefinition.command shorthand and the inner
    // HookType::Command.command in the legacy old Settings shape.
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
            Some(super::run::HookTlsMode::Verify) => OldTlsMode::Verify,
            Some(super::run::HookTlsMode::NoVerify) => OldTlsMode::NoVerify,
            Some(super::run::HookTlsMode::Off) => OldTlsMode::Off,
            None => OldTlsMode::default(),
        };
        return Some(OldHookType::Http {
            url: interp_to_string(url),
            headers,
            allowed_env_vars: hook.allowed_env_vars.clone(),
            tls,
        });
    }
    if hook.agent.is_some() {
        return Some(OldHookType::Agent {
            prompt: hook
                .prompt
                .as_ref()
                .map(interp_to_string)
                .unwrap_or_default(),
            model: hook.model.as_ref().map(interp_to_string),
            max_tool_rounds: hook.max_tool_rounds,
        });
    }
    hook.prompt.as_ref().map(|prompt| OldHookType::Prompt {
        prompt: interp_to_string(prompt),
        model: hook.model.as_ref().map(interp_to_string),
    })
}

fn bridge_hook_event(event: V2HookEvent) -> OldHookEvent {
    match event {
        V2HookEvent::RunStart => OldHookEvent::RunStart,
        V2HookEvent::RunComplete => OldHookEvent::RunComplete,
        V2HookEvent::RunFailed => OldHookEvent::RunFailed,
        V2HookEvent::StageStart => OldHookEvent::StageStart,
        V2HookEvent::StageComplete => OldHookEvent::StageComplete,
        V2HookEvent::StageFailed => OldHookEvent::StageFailed,
        V2HookEvent::StageRetrying => OldHookEvent::StageRetrying,
        V2HookEvent::EdgeSelected => OldHookEvent::EdgeSelected,
        V2HookEvent::ParallelStart => OldHookEvent::ParallelStart,
        V2HookEvent::ParallelComplete => OldHookEvent::ParallelComplete,
        V2HookEvent::SandboxReady => OldHookEvent::SandboxReady,
        V2HookEvent::SandboxCleanup => OldHookEvent::SandboxCleanup,
        V2HookEvent::CheckpointSaved => OldHookEvent::CheckpointSaved,
        V2HookEvent::PreToolUse => OldHookEvent::PreToolUse,
        V2HookEvent::PostToolUse => OldHookEvent::PostToolUse,
        V2HookEvent::PostToolUseFailure => OldHookEvent::PostToolUseFailure,
    }
}

fn interp_to_string(value: &InterpString) -> String {
    value.as_source()
}

fn size_to_gb_i32(bytes: u64) -> i32 {
    let gb = bytes / 1_000_000_000;
    i32::try_from(gb).unwrap_or(i32::MAX)
}
