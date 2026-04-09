//! Temporary bridge from the v2 parse tree to the old flat [`Settings`] shape.
//!
//! This module exists only to keep consumers compiling while Stages 3 and 4
//! migrate parsers and consumers across the workspace. Field mappings are
//! best-effort and deliberately lossy for anything the old shape does not
//! have a slot for. **This entire module is deleted in Stage 6.**
//!
//! Env var interpolation is not performed here; `${env.NAME}` tokens are
//! emitted verbatim via [`InterpString::as_source`]. The post-layering
//! interpolation pass runs in `fabro-config` during Stage 3, after layering
//! is already complete.

use std::collections::HashMap;

use super::cli::{CliExecLayer, CliLayer, CliOutputLayer, CliTargetLayer, OutputVerbosity};
use super::interp::InterpString;
use super::project::ProjectLayer;
use super::run::{
    AgentPermissions as V2AgentPermissions, ApprovalMode, HookEntry as V2HookEntry,
    HookEvent as V2HookEvent, McpEntryLayer, MergeStrategy as V2MergeStrategy, ModelRefOrSplice,
    RunLayer, RunMode, WorktreeMode as V2WorktreeMode,
};
use super::server::{
    ObjectStoreProvider, ServerArtifactsLayer, ServerIntegrationsLayer, ServerLayer,
    ServerSchedulerLayer, ServerStorageLayer, ServerWebLayer,
};
use super::tree::SettingsFile;
use super::workflow::WorkflowLayer;
use crate::settings::Settings;
use crate::settings::hook::{
    HookDefinition, HookEvent as OldHookEvent, HookType as OldHookType, TlsMode as OldTlsMode,
};
use crate::settings::mcp::{McpServerEntry, McpTransport};
use crate::settings::project::ProjectSettings;
use crate::settings::run::{
    ArtifactsSettings, CheckpointSettings, LlmSettings, MergeStrategy as OldMergeStrategy,
    PullRequestSettings, SetupSettings,
};
use crate::settings::sandbox::{
    DaytonaNetwork, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    LocalSandboxSettings, SandboxSettings, WorktreeMode as OldWorktreeMode,
};
use crate::settings::server::{
    ApiAuthStrategy, ApiSettings, ArtifactStorageBackend, ArtifactStorageSettings, AuthProvider,
    AuthSettings, FeaturesSettings, GitAuthorSettings, GitProvider, GitSettings, LogSettings,
    SlackSettings, WebSettings,
};
use crate::settings::user::{
    ExecSettings, OutputFormat, PermissionLevel, ServerSettings as UserServer,
};

/// Convert a v2 `SettingsFile` into the legacy flat [`Settings`] shape.
///
/// This is a temporary seam. All v2 fields that do not map cleanly are
/// dropped; callers that need those should read the v2 tree directly.
#[must_use]
pub fn bridge_to_old(file: &SettingsFile) -> Settings {
    let mut out = Settings {
        version: file.version,
        ..Settings::default()
    };

    if let Some(project) = &file.project {
        bridge_project(project, &mut out);
    }
    if let Some(workflow) = &file.workflow {
        bridge_workflow(workflow, &mut out);
    }
    if let Some(run) = &file.run {
        bridge_run(run, &mut out);
    }
    if let Some(cli) = &file.cli {
        bridge_cli(cli, &mut out);
    }
    if let Some(server) = &file.server {
        bridge_server(server, &mut out);
    }
    if let Some(features) = &file.features {
        out.features = Some(FeaturesSettings {
            session_sandboxes: features.session_sandboxes.unwrap_or(false),
            retros: false, // v2 moves retros to run.execution.retros
        });
    }

    out
}

fn bridge_project(project: &ProjectLayer, out: &mut Settings) {
    if let Some(directory) = &project.directory {
        out.fabro = Some(ProjectSettings {
            root: directory.clone(),
        });
    }
    if !project.metadata.is_empty() {
        merge_labels(&mut out.labels, &project.metadata);
    }
}

fn bridge_workflow(workflow: &WorkflowLayer, out: &mut Settings) {
    if let Some(graph) = &workflow.graph {
        out.graph = Some(graph.clone());
    }
    if !workflow.metadata.is_empty() {
        merge_labels(&mut out.labels, &workflow.metadata);
    }
}

fn bridge_run(run: &RunLayer, out: &mut Settings) {
    if let Some(goal) = &run.goal {
        out.goal = Some(interp_to_string(goal));
    }
    if let Some(wd) = &run.working_dir {
        out.work_dir = Some(interp_to_string(wd));
    }
    if !run.metadata.is_empty() {
        merge_labels(&mut out.labels, &run.metadata);
    }

    if let Some(inputs) = &run.inputs {
        let mut vars: HashMap<String, String> = HashMap::new();
        for (k, v) in inputs {
            vars.insert(k.clone(), toml_value_to_string(v));
        }
        out.vars = Some(vars);
    }

    if let Some(model) = &run.model {
        let mut llm = LlmSettings::default();
        if let Some(p) = &model.provider {
            llm.provider = Some(interp_to_string(p));
        }
        if let Some(n) = &model.name {
            llm.model = Some(interp_to_string(n));
        }
        if !model.fallbacks.is_empty() {
            let mut fallbacks_by_provider: HashMap<String, Vec<String>> = HashMap::new();
            for entry in &model.fallbacks {
                match entry {
                    ModelRefOrSplice::ModelRef(model_ref) => {
                        let s = model_ref.to_string();
                        fallbacks_by_provider
                            .entry(String::new())
                            .or_default()
                            .push(s);
                    }
                    ModelRefOrSplice::Splice => {}
                }
            }
            if !fallbacks_by_provider.is_empty() {
                llm.fallbacks = Some(fallbacks_by_provider);
            }
        }
        out.llm = Some(llm);
    }

    if let Some(prepare) = &run.prepare {
        let commands: Vec<String> = prepare
            .steps
            .iter()
            .filter_map(|step| {
                if let Some(script) = &step.script {
                    Some(interp_to_string(script))
                } else {
                    step.command.as_ref().map(|argv| {
                        argv.iter()
                            .map(interp_to_string)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                }
            })
            .collect();
        let timeout_ms = prepare
            .timeout
            .map(|d| u64::try_from(d.as_std().as_millis()).unwrap_or(u64::MAX));
        out.setup = Some(SetupSettings {
            commands,
            timeout_ms,
        });
    }

    if let Some(execution) = &run.execution {
        out.dry_run = match execution.mode {
            Some(RunMode::DryRun) => Some(true),
            Some(RunMode::Normal) => Some(false),
            None => None,
        };
        out.auto_approve = match execution.approval {
            Some(ApprovalMode::Auto) => Some(true),
            Some(ApprovalMode::Prompt) => Some(false),
            None => None,
        };
        out.no_retro = execution.retros.map(|r| !r);
    }

    if let Some(cp) = &run.checkpoint {
        out.checkpoint = CheckpointSettings {
            exclude_globs: cp.exclude_globs.clone(),
        };
    }

    if let Some(sb) = &run.sandbox {
        out.sandbox = Some(bridge_sandbox(sb));
    }

    if let Some(agent) = &run.agent {
        let map = bridge_mcps(&agent.mcps);
        if !map.is_empty() {
            out.mcp_servers = map;
        }
    }

    if !run.hooks.is_empty() {
        out.hooks = run.hooks.iter().map(bridge_hook).collect();
    }

    if let Some(pr) = &run.pull_request {
        out.pull_request = Some(PullRequestSettings {
            enabled: pr.enabled.unwrap_or(false),
            draft: pr.draft.unwrap_or(true),
            auto_merge: pr.auto_merge.unwrap_or(false),
            merge_strategy: pr
                .merge_strategy
                .map(bridge_merge_strategy)
                .unwrap_or_default(),
        });
    }

    if let Some(art) = &run.artifacts {
        out.artifacts = Some(ArtifactsSettings {
            include: art.include.clone(),
        });
    }

    // Slack notifications feed the old flat SlackSettings.default_channel.
    for route in run.notifications.values() {
        if let Some(slack) = &route.slack {
            if let Some(channel) = &slack.channel {
                out.slack
                    .get_or_insert_with(SlackSettings::default)
                    .default_channel = Some(interp_to_string(channel));
                break;
            }
        }
    }

    // Git author from run.git
    if let Some(git) = &run.git {
        if let Some(author) = &git.author {
            let git_settings = out.git.get_or_insert_with(GitSettings::default);
            git_settings.author = GitAuthorSettings {
                name: author.name.as_ref().map(interp_to_string),
                email: author.email.as_ref().map(interp_to_string),
            };
        }
    }
}

fn bridge_sandbox(sb: &super::run::RunSandboxLayer) -> SandboxSettings {
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

fn bridge_worktree_mode(m: V2WorktreeMode) -> OldWorktreeMode {
    match m {
        V2WorktreeMode::Always => OldWorktreeMode::Always,
        V2WorktreeMode::Clean => OldWorktreeMode::Clean,
        V2WorktreeMode::Dirty => OldWorktreeMode::Dirty,
        V2WorktreeMode::Never => OldWorktreeMode::Never,
    }
}

fn bridge_merge_strategy(m: V2MergeStrategy) -> OldMergeStrategy {
    match m {
        V2MergeStrategy::Squash => OldMergeStrategy::Squash,
        V2MergeStrategy::Merge => OldMergeStrategy::Merge,
        V2MergeStrategy::Rebase => OldMergeStrategy::Rebase,
    }
}

fn bridge_mcps(mcps: &HashMap<String, McpEntryLayer>) -> HashMap<String, McpServerEntry> {
    mcps.iter()
        .map(|(name, entry)| (name.clone(), bridge_mcp_entry(entry)))
        .collect()
}

fn bridge_mcp_entry(entry: &McpEntryLayer) -> McpServerEntry {
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

fn bridge_hook(hook: &V2HookEntry) -> HookDefinition {
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

fn bridge_cli(cli: &CliLayer, out: &mut Settings) {
    if let Some(target) = &cli.target {
        let target_str = match target {
            CliTargetLayer::Http { url, .. } => url.as_ref().map(interp_to_string),
            CliTargetLayer::Unix { path } => path.as_ref().map(interp_to_string),
        };
        if target_str.is_some() {
            out.server = Some(UserServer {
                target: target_str,
                tls: None,
            });
        }
    }

    if let Some(exec) = &cli.exec {
        out.exec = Some(bridge_exec(exec));
        if let Some(idle) = exec.prevent_idle_sleep {
            out.prevent_idle_sleep = Some(idle);
        }
    }

    if let Some(output) = &cli.output {
        bridge_cli_output(output, out);
    }

    if let Some(updates) = &cli.updates {
        out.upgrade_check = updates.check;
    }
}

fn bridge_exec(exec: &CliExecLayer) -> ExecSettings {
    ExecSettings {
        provider: exec
            .model
            .as_ref()
            .and_then(|m| m.provider.as_ref())
            .map(interp_to_string),
        model: exec
            .model
            .as_ref()
            .and_then(|m| m.name.as_ref())
            .map(interp_to_string),
        permissions: exec.agent.as_ref().and_then(|a| {
            a.permissions.map(|p| match p {
                V2AgentPermissions::ReadOnly => PermissionLevel::ReadOnly,
                V2AgentPermissions::ReadWrite => PermissionLevel::ReadWrite,
                V2AgentPermissions::Full => PermissionLevel::Full,
            })
        }),
        output_format: None,
    }
}

fn bridge_cli_output(output: &CliOutputLayer, out: &mut Settings) {
    if let Some(format) = output.format {
        let fmt = match format {
            super::cli::OutputFormat::Text => OutputFormat::Text,
            super::cli::OutputFormat::Json => OutputFormat::Json,
        };
        out.exec
            .get_or_insert_with(ExecSettings::default)
            .output_format = Some(fmt);
    }
    if let Some(verbosity) = output.verbosity {
        out.verbose = Some(matches!(verbosity, OutputVerbosity::Verbose));
    }
}

fn bridge_server(server: &ServerLayer, out: &mut Settings) {
    if let Some(storage) = &server.storage {
        bridge_storage(storage, out);
    }
    if let Some(scheduler) = &server.scheduler {
        bridge_scheduler(scheduler, out);
    }
    if let Some(artifacts) = &server.artifacts {
        out.artifact_storage = Some(bridge_artifacts(artifacts));
    }
    if let Some(web) = &server.web {
        out.web = Some(bridge_web(web));
    }
    if let Some(api) = &server.api {
        out.api = Some(ApiSettings {
            base_url: api.url.as_ref().map_or_else(
                || "http://localhost:3000/api/v1".to_string(),
                interp_to_string,
            ),
            authentication_strategies: bridge_api_auth_strategies(server.auth.as_ref()),
            tls: None,
        });
    }
    if let Some(logging) = &server.logging {
        out.log = Some(LogSettings {
            level: logging.level.clone(),
        });
    }
    if let Some(integrations) = &server.integrations {
        bridge_integrations(integrations, out);
    }
}

fn bridge_storage(storage: &ServerStorageLayer, out: &mut Settings) {
    if let Some(root) = &storage.root {
        out.storage_dir = Some(std::path::PathBuf::from(interp_to_string(root)));
    }
}

fn bridge_scheduler(scheduler: &ServerSchedulerLayer, out: &mut Settings) {
    out.max_concurrent_runs = scheduler.max_concurrent_runs;
}

fn bridge_artifacts(a: &ServerArtifactsLayer) -> ArtifactStorageSettings {
    let backend = match a.provider {
        Some(ObjectStoreProvider::Local) | None => ArtifactStorageBackend::Local,
        Some(ObjectStoreProvider::S3) => ArtifactStorageBackend::S3,
    };
    let prefix = a
        .prefix
        .as_ref()
        .map_or_else(|| "artifacts".to_string(), interp_to_string);
    let (bucket, region, endpoint, path_style) =
        a.s3.as_ref().map_or((None, None, None, None), |s3| {
            (
                s3.bucket.as_ref().map(interp_to_string),
                s3.region.as_ref().map(interp_to_string),
                s3.endpoint.as_ref().map(interp_to_string),
                s3.path_style,
            )
        });
    ArtifactStorageSettings {
        backend,
        prefix,
        bucket,
        region,
        endpoint,
        path_style,
    }
}

fn bridge_web(web: &ServerWebLayer) -> WebSettings {
    WebSettings {
        enabled: web.enabled.unwrap_or(true),
        url: web
            .url
            .as_ref()
            .map_or_else(|| "http://localhost:3000".to_string(), interp_to_string),
        auth: AuthSettings {
            provider: AuthProvider::Github,
            allowed_usernames: Vec::new(),
        },
    }
}

fn bridge_api_auth_strategies(
    auth: Option<&super::server::ServerAuthLayer>,
) -> Vec<ApiAuthStrategy> {
    let Some(auth) = auth else {
        return Vec::new();
    };
    let Some(api) = &auth.api else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(jwt) = &api.jwt {
        if jwt.enabled.unwrap_or(true) {
            out.push(ApiAuthStrategy::Jwt);
        }
    }
    if let Some(mtls) = &api.mtls {
        if mtls.enabled.unwrap_or(true) {
            out.push(ApiAuthStrategy::Mtls);
        }
    }
    out
}

fn bridge_integrations(integrations: &ServerIntegrationsLayer, out: &mut Settings) {
    if let Some(github) = &integrations.github {
        let git_settings = out.git.get_or_insert_with(|| GitSettings {
            provider: GitProvider::Github,
            ..GitSettings::default()
        });
        if let Some(id) = &github.app_id {
            git_settings.app_id = Some(interp_to_string(id));
        }
        if let Some(cid) = &github.client_id {
            git_settings.client_id = Some(interp_to_string(cid));
        }
        if let Some(slug) = &github.slug {
            git_settings.slug = Some(interp_to_string(slug));
        }
    }
    if let Some(slack) = &integrations.slack {
        let slack_settings = out.slack.get_or_insert_with(SlackSettings::default);
        if let Some(channel) = &slack.default_channel {
            slack_settings.default_channel = Some(interp_to_string(channel));
        }
    }
}

// ------------------- shared helpers -------------------

fn merge_labels(out: &mut HashMap<String, String>, src: &HashMap<String, String>) {
    for (k, v) in src {
        out.insert(k.clone(), v.clone());
    }
}

fn interp_to_string(value: &InterpString) -> String {
    value.as_source()
}

fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn size_to_gb_i32(bytes: u64) -> i32 {
    let gb = bytes / 1_000_000_000;
    i32::try_from(gb).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_bridges_to_empty_settings() {
        let file = SettingsFile::default();
        let old = bridge_to_old(&file);
        assert_eq!(old.goal, None);
        assert_eq!(old.vars, None);
    }

    #[test]
    fn run_goal_bridges_to_old_goal() {
        let file = SettingsFile {
            run: Some(RunLayer {
                goal: Some(InterpString::parse("Implement OAuth")),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };
        let old = bridge_to_old(&file);
        assert_eq!(old.goal.as_deref(), Some("Implement OAuth"));
    }

    #[test]
    fn project_directory_bridges_to_old_fabro_root() {
        let file = SettingsFile {
            project: Some(ProjectLayer {
                directory: Some("fabro/".into()),
                ..ProjectLayer::default()
            }),
            ..SettingsFile::default()
        };
        let old = bridge_to_old(&file);
        assert_eq!(old.fabro.as_ref().map(|f| f.root.as_str()), Some("fabro/"));
    }

    #[test]
    fn run_execution_dry_run_bridges_to_old_dry_run_true() {
        use super::super::run::{RunExecutionLayer, RunMode};
        let file = SettingsFile {
            run: Some(RunLayer {
                execution: Some(RunExecutionLayer {
                    mode: Some(RunMode::DryRun),
                    ..RunExecutionLayer::default()
                }),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };
        let old = bridge_to_old(&file);
        assert_eq!(old.dry_run, Some(true));
    }

    #[test]
    fn run_execution_retros_true_bridges_to_old_no_retro_false() {
        use super::super::run::RunExecutionLayer;
        let file = SettingsFile {
            run: Some(RunLayer {
                execution: Some(RunExecutionLayer {
                    retros: Some(true),
                    ..RunExecutionLayer::default()
                }),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };
        let old = bridge_to_old(&file);
        assert_eq!(old.no_retro, Some(false));
    }
}
