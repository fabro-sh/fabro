//! v2 merge matrix implementation.
//!
//! Encodes the normative merge behavior from the requirements doc: replace
//! scalars, field-merge structured tables, replace freeform maps by default,
//! sticky merge-by-key where the requirements call for it, splice-capable
//! string arrays, whole-list replacement for ordered prepare steps, and
//! ordered hook merging with optional `id` replacement.
#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;

use fabro_types::settings::cli::{
    CliExecAgentLayer, CliExecLayer, CliExecModelLayer, CliLayer, CliTargetLayer,
};
use fabro_types::settings::layer::SettingsLayer;
use fabro_types::settings::project::ProjectLayer;
use fabro_types::settings::run::{
    DaytonaSandboxLayer, GitAuthorLayer, HookEntry, InterviewsLayer, ModelRefOrSplice,
    NotificationRouteLayer, RunAgentLayer, RunCheckpointLayer, RunExecutionLayer, RunGitLayer,
    RunLayer, RunModelLayer, RunPrepareLayer, RunPullRequestLayer, RunSandboxLayer, RunScmLayer,
    StringOrSplice,
};
use fabro_types::settings::server::{
    ServerArtifactsLayer, ServerAuthLayer, ServerIntegrationsLayer, ServerLayer, ServerListenLayer,
    ServerSchedulerLayer, ServerSlateDbLayer, ServerStorageLayer, ServerWebLayer,
};
use fabro_types::settings::workflow::WorkflowLayer;

/// Combine two settings files: `higher` takes precedence over `lower` wherever
/// the merge matrix does not dictate otherwise.
#[must_use]
pub fn combine_files(lower: SettingsLayer, higher: SettingsLayer) -> SettingsLayer {
    SettingsLayer {
        version:  higher.version.or(lower.version),
        project:  merge_option(lower.project, higher.project, combine_project),
        workflow: merge_option(lower.workflow, higher.workflow, combine_workflow),
        run:      merge_option(lower.run, higher.run, combine_run),
        cli:      merge_option(lower.cli, higher.cli, combine_cli),
        server:   merge_option(lower.server, higher.server, combine_server),
        features: replace_if_some(lower.features, higher.features),
    }
}

fn merge_option<T>(lower: Option<T>, higher: Option<T>, f: fn(T, T) -> T) -> Option<T> {
    match (lower, higher) {
        (Some(l), Some(h)) => Some(f(l, h)),
        (Some(l), None) => Some(l),
        (None, Some(h)) => Some(h),
        (None, None) => None,
    }
}

fn replace_if_some<T>(lower: Option<T>, higher: Option<T>) -> Option<T> {
    higher.or(lower)
}

fn merge_string_map_replace(
    lower: HashMap<String, String>,
    higher: HashMap<String, String>,
) -> HashMap<String, String> {
    if higher.is_empty() { lower } else { higher }
}

fn merge_string_map_sticky<T>(
    mut lower: HashMap<String, T>,
    higher: HashMap<String, T>,
) -> HashMap<String, T> {
    for (k, v) in higher {
        lower.insert(k, v);
    }
    lower
}

// ------------------- project -------------------

fn combine_project(lower: ProjectLayer, higher: ProjectLayer) -> ProjectLayer {
    ProjectLayer {
        name:        higher.name.or(lower.name),
        description: higher.description.or(lower.description),
        directory:   higher.directory.or(lower.directory),
        metadata:    merge_string_map_replace(lower.metadata, higher.metadata),
    }
}

// ------------------- workflow -------------------

fn combine_workflow(lower: WorkflowLayer, higher: WorkflowLayer) -> WorkflowLayer {
    WorkflowLayer {
        name:        higher.name.or(lower.name),
        description: higher.description.or(lower.description),
        graph:       higher.graph.or(lower.graph),
        metadata:    merge_string_map_replace(lower.metadata, higher.metadata),
    }
}

// ------------------- run -------------------

fn combine_run(lower: RunLayer, higher: RunLayer) -> RunLayer {
    RunLayer {
        goal:          higher.goal.or(lower.goal),
        working_dir:   higher.working_dir.or(lower.working_dir),
        metadata:      merge_string_map_replace(lower.metadata, higher.metadata),
        inputs:        higher.inputs.or(lower.inputs),
        model:         merge_option(lower.model, higher.model, combine_run_model),
        git:           merge_option(lower.git, higher.git, combine_run_git),
        prepare:       merge_option(lower.prepare, higher.prepare, combine_run_prepare),
        execution:     merge_option(lower.execution, higher.execution, combine_run_execution),
        checkpoint:    merge_option(lower.checkpoint, higher.checkpoint, combine_run_checkpoint),
        sandbox:       merge_option(lower.sandbox, higher.sandbox, combine_run_sandbox),
        notifications: combine_notifications(lower.notifications, higher.notifications),
        interviews:    merge_option(lower.interviews, higher.interviews, combine_interviews),
        agent:         merge_option(lower.agent, higher.agent, combine_run_agent),
        hooks:         combine_hooks(lower.hooks, higher.hooks),
        scm:           merge_option(lower.scm, higher.scm, combine_run_scm),
        pull_request:  merge_option(lower.pull_request, higher.pull_request, combine_run_pr),
        artifacts:     replace_if_some(lower.artifacts, higher.artifacts),
    }
}

fn combine_run_model(lower: RunModelLayer, higher: RunModelLayer) -> RunModelLayer {
    RunModelLayer {
        provider:  higher.provider.or(lower.provider),
        name:      higher.name.or(lower.name),
        fallbacks: splice_model_fallbacks(lower.fallbacks, higher.fallbacks),
    }
}

fn splice_model_fallbacks(
    lower: Vec<ModelRefOrSplice>,
    higher: Vec<ModelRefOrSplice>,
) -> Vec<ModelRefOrSplice> {
    if higher.is_empty() {
        return lower;
    }
    let splice_pos = higher
        .iter()
        .position(|e| matches!(e, ModelRefOrSplice::Splice));
    let Some(pos) = splice_pos else {
        return higher;
    };
    let mut out = Vec::new();
    for (i, entry) in higher.into_iter().enumerate() {
        if i == pos {
            out.extend(
                lower
                    .iter()
                    .filter(|e| !matches!(e, ModelRefOrSplice::Splice))
                    .cloned(),
            );
        } else if !matches!(entry, ModelRefOrSplice::Splice) {
            out.push(entry);
        }
    }
    out
}

fn combine_run_git(lower: RunGitLayer, higher: RunGitLayer) -> RunGitLayer {
    RunGitLayer {
        author: merge_option(lower.author, higher.author, combine_git_author),
    }
}

fn combine_git_author(lower: GitAuthorLayer, higher: GitAuthorLayer) -> GitAuthorLayer {
    GitAuthorLayer {
        name:  higher.name.or(lower.name),
        email: higher.email.or(lower.email),
    }
}

fn combine_run_prepare(_lower: RunPrepareLayer, higher: RunPrepareLayer) -> RunPrepareLayer {
    // Whole-list replacement for prepare.steps per the merge matrix.
    higher
}

fn combine_run_execution(lower: RunExecutionLayer, higher: RunExecutionLayer) -> RunExecutionLayer {
    RunExecutionLayer {
        mode:     higher.mode.or(lower.mode),
        approval: higher.approval.or(lower.approval),
        retros:   higher.retros.or(lower.retros),
    }
}

fn combine_run_checkpoint(
    lower: RunCheckpointLayer,
    higher: RunCheckpointLayer,
) -> RunCheckpointLayer {
    // Exclude globs are a security/policy list: replace by default.
    if higher.exclude_globs.is_empty() {
        lower
    } else {
        higher
    }
}

fn combine_run_sandbox(lower: RunSandboxLayer, higher: RunSandboxLayer) -> RunSandboxLayer {
    RunSandboxLayer {
        provider:     higher.provider.or(lower.provider),
        preserve:     higher.preserve.or(lower.preserve),
        devcontainer: higher.devcontainer.or(lower.devcontainer),
        // Sticky merge-by-key for run.sandbox.env per R71.
        env:          merge_string_map_sticky(lower.env, higher.env),
        local:        higher.local.or(lower.local),
        daytona:      merge_option(lower.daytona, higher.daytona, combine_daytona),
    }
}

fn combine_daytona(lower: DaytonaSandboxLayer, higher: DaytonaSandboxLayer) -> DaytonaSandboxLayer {
    DaytonaSandboxLayer {
        auto_stop_interval: higher.auto_stop_interval.or(lower.auto_stop_interval),
        // Sticky merge-by-key for provider-native labels per R71.
        labels:             merge_string_map_sticky(lower.labels, higher.labels),
        snapshot:           higher.snapshot.or(lower.snapshot),
        network:            higher.network.or(lower.network),
        skip_clone:         higher.skip_clone.or(lower.skip_clone),
    }
}

fn combine_notifications(
    mut lower: HashMap<String, NotificationRouteLayer>,
    higher: HashMap<String, NotificationRouteLayer>,
) -> HashMap<String, NotificationRouteLayer> {
    for (k, h) in higher {
        match lower.remove(&k) {
            Some(l) => {
                lower.insert(k, combine_notification_route(l, h));
            }
            None => {
                lower.insert(k, h);
            }
        }
    }
    lower
}

fn combine_notification_route(
    lower: NotificationRouteLayer,
    higher: NotificationRouteLayer,
) -> NotificationRouteLayer {
    NotificationRouteLayer {
        enabled:  higher.enabled.or(lower.enabled),
        provider: higher.provider.or(lower.provider),
        events:   splice_events(lower.events, higher.events),
        slack:    higher.slack.or(lower.slack),
        discord:  higher.discord.or(lower.discord),
        teams:    higher.teams.or(lower.teams),
    }
}

fn splice_events(lower: Vec<StringOrSplice>, higher: Vec<StringOrSplice>) -> Vec<StringOrSplice> {
    if higher.is_empty() {
        return lower;
    }
    let splice_pos = higher
        .iter()
        .position(|e| matches!(e, StringOrSplice::Splice));
    let Some(pos) = splice_pos else {
        return higher;
    };
    let mut out = Vec::new();
    for (i, entry) in higher.into_iter().enumerate() {
        if i == pos {
            out.extend(
                lower
                    .iter()
                    .filter(|e| !matches!(e, StringOrSplice::Splice))
                    .cloned(),
            );
        } else if !matches!(entry, StringOrSplice::Splice) {
            out.push(entry);
        }
    }
    out
}

fn combine_interviews(lower: InterviewsLayer, higher: InterviewsLayer) -> InterviewsLayer {
    InterviewsLayer {
        provider: higher.provider.or(lower.provider),
        slack:    higher.slack.or(lower.slack),
        discord:  higher.discord.or(lower.discord),
        teams:    higher.teams.or(lower.teams),
    }
}

fn combine_run_agent(lower: RunAgentLayer, higher: RunAgentLayer) -> RunAgentLayer {
    RunAgentLayer {
        permissions: higher.permissions.or(lower.permissions),
        // MCP entries: field-merge per key. Higher replaces lower for same keys.
        mcps:        merge_string_map_sticky(lower.mcps, higher.mcps),
    }
}

/// Merge two ordered hook lists using the id-aware replacement rule.
fn combine_hooks(lower: Vec<HookEntry>, higher: Vec<HookEntry>) -> Vec<HookEntry> {
    let mut out: Vec<HookEntry> = Vec::with_capacity(lower.len() + higher.len());
    let mut appended_ids: Vec<String> = Vec::new();

    for lower_entry in &lower {
        if let Some(id) = &lower_entry.id {
            if let Some(replacement) = higher.iter().find(|h| h.id.as_deref() == Some(id.as_str()))
            {
                out.push(replacement.clone());
                appended_ids.push(id.clone());
                continue;
            }
        }
        out.push(lower_entry.clone());
    }

    for higher_entry in higher {
        if let Some(id) = &higher_entry.id {
            if appended_ids.contains(id) {
                continue;
            }
        }
        out.push(higher_entry);
    }

    out
}

fn combine_run_scm(lower: RunScmLayer, higher: RunScmLayer) -> RunScmLayer {
    RunScmLayer {
        provider:   higher.provider.or(lower.provider),
        owner:      higher.owner.or(lower.owner),
        repository: higher.repository.or(lower.repository),
        github:     higher.github.or(lower.github),
    }
}

fn combine_run_pr(lower: RunPullRequestLayer, higher: RunPullRequestLayer) -> RunPullRequestLayer {
    RunPullRequestLayer {
        enabled:        higher.enabled.or(lower.enabled),
        draft:          higher.draft.or(lower.draft),
        auto_merge:     higher.auto_merge.or(lower.auto_merge),
        merge_strategy: higher.merge_strategy.or(lower.merge_strategy),
    }
}

// ------------------- cli -------------------

fn combine_cli(lower: CliLayer, higher: CliLayer) -> CliLayer {
    CliLayer {
        target:  merge_option(lower.target, higher.target, combine_cli_target),
        auth:    higher.auth.or(lower.auth),
        exec:    merge_option(lower.exec, higher.exec, combine_cli_exec),
        output:  higher.output.or(lower.output),
        updates: higher.updates.or(lower.updates),
        logging: higher.logging.or(lower.logging),
    }
}

fn combine_cli_target(_lower: CliTargetLayer, higher: CliTargetLayer) -> CliTargetLayer {
    // The transport type is a scalar discriminant: the higher layer's choice wins.
    higher
}

fn combine_cli_exec(lower: CliExecLayer, higher: CliExecLayer) -> CliExecLayer {
    CliExecLayer {
        prevent_idle_sleep: higher.prevent_idle_sleep.or(lower.prevent_idle_sleep),
        model:              merge_option(lower.model, higher.model, combine_cli_exec_model),
        agent:              merge_option(lower.agent, higher.agent, combine_cli_exec_agent),
    }
}

fn combine_cli_exec_model(
    lower: CliExecModelLayer,
    higher: CliExecModelLayer,
) -> CliExecModelLayer {
    CliExecModelLayer {
        provider: higher.provider.or(lower.provider),
        name:     higher.name.or(lower.name),
    }
}

fn combine_cli_exec_agent(
    lower: CliExecAgentLayer,
    higher: CliExecAgentLayer,
) -> CliExecAgentLayer {
    CliExecAgentLayer {
        permissions: higher.permissions.or(lower.permissions),
        mcps:        merge_string_map_sticky(lower.mcps, higher.mcps),
    }
}

// ------------------- server -------------------

fn combine_server(lower: ServerLayer, higher: ServerLayer) -> ServerLayer {
    ServerLayer {
        listen:       merge_option(lower.listen, higher.listen, combine_listen),
        api:          higher.api.or(lower.api),
        web:          merge_option(lower.web, higher.web, combine_server_web),
        auth:         merge_option(lower.auth, higher.auth, combine_server_auth),
        storage:      merge_option(lower.storage, higher.storage, combine_server_storage),
        artifacts:    merge_option(lower.artifacts, higher.artifacts, combine_server_artifacts),
        slatedb:      merge_option(lower.slatedb, higher.slatedb, combine_server_slatedb),
        scheduler:    merge_option(lower.scheduler, higher.scheduler, combine_server_scheduler),
        logging:      higher.logging.or(lower.logging),
        integrations: merge_option(
            lower.integrations,
            higher.integrations,
            combine_server_integrations,
        ),
    }
}

fn combine_listen(_lower: ServerListenLayer, higher: ServerListenLayer) -> ServerListenLayer {
    // Transport type is a scalar discriminant: replace whole.
    higher
}

fn combine_server_web(lower: ServerWebLayer, higher: ServerWebLayer) -> ServerWebLayer {
    ServerWebLayer {
        enabled: higher.enabled.or(lower.enabled),
        url:     higher.url.or(lower.url),
    }
}

fn combine_server_auth(lower: ServerAuthLayer, higher: ServerAuthLayer) -> ServerAuthLayer {
    ServerAuthLayer {
        api: higher.api.or(lower.api),
        web: higher.web.or(lower.web),
    }
}

fn combine_server_storage(
    lower: ServerStorageLayer,
    higher: ServerStorageLayer,
) -> ServerStorageLayer {
    ServerStorageLayer {
        root: higher.root.or(lower.root),
    }
}

fn combine_server_artifacts(
    lower: ServerArtifactsLayer,
    higher: ServerArtifactsLayer,
) -> ServerArtifactsLayer {
    ServerArtifactsLayer {
        provider: higher.provider.or(lower.provider),
        prefix:   higher.prefix.or(lower.prefix),
        local:    higher.local.or(lower.local),
        s3:       higher.s3.or(lower.s3),
    }
}

fn combine_server_slatedb(
    lower: ServerSlateDbLayer,
    higher: ServerSlateDbLayer,
) -> ServerSlateDbLayer {
    ServerSlateDbLayer {
        provider:       higher.provider.or(lower.provider),
        prefix:         higher.prefix.or(lower.prefix),
        flush_interval: higher.flush_interval.or(lower.flush_interval),
        local:          higher.local.or(lower.local),
        s3:             higher.s3.or(lower.s3),
    }
}

fn combine_server_scheduler(
    lower: ServerSchedulerLayer,
    higher: ServerSchedulerLayer,
) -> ServerSchedulerLayer {
    ServerSchedulerLayer {
        max_concurrent_runs: higher.max_concurrent_runs.or(lower.max_concurrent_runs),
    }
}

fn combine_server_integrations(
    lower: ServerIntegrationsLayer,
    higher: ServerIntegrationsLayer,
) -> ServerIntegrationsLayer {
    ServerIntegrationsLayer {
        github:  higher.github.or(lower.github),
        slack:   higher.slack.or(lower.slack),
        discord: higher.discord.or(lower.discord),
        teams:   higher.teams.or(lower.teams),
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::InterpString;

    use super::*;
    use crate::parse::parse_settings_layer;

    fn parse(input: &str) -> SettingsLayer {
        parse_settings_layer(input).expect("fixture should parse")
    }

    #[test]
    fn run_inputs_replace_wholesale() {
        let lower = parse(
            r#"
[run.inputs]
a = "lower"
b = "lower"
"#,
        );
        let higher = parse(
            r#"
[run.inputs]
a = "higher"
"#,
        );
        let merged = combine_files(lower, higher);
        let inputs = merged.run.unwrap().inputs.unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs.get("a"), Some(&toml::Value::String("higher".into())));
        assert!(!inputs.contains_key("b"), "lower key should be gone");
    }

    #[test]
    fn run_sandbox_env_merges_sticky() {
        let lower = parse(
            r#"
[run.sandbox.env]
A = "lower-a"
B = "lower-b"
"#,
        );
        let higher = parse(
            r#"
[run.sandbox.env]
A = "higher-a"
C = "higher-c"
"#,
        );
        let merged = combine_files(lower, higher);
        let sandbox = merged.run.unwrap().sandbox.unwrap();
        assert_eq!(sandbox.env.len(), 3);
    }

    #[test]
    fn run_prepare_steps_replaces_whole_list() {
        let lower = parse(
            r#"
[[run.prepare.steps]]
script = "lower-1"

[[run.prepare.steps]]
script = "lower-2"
"#,
        );
        let higher = parse(
            r#"
[[run.prepare.steps]]
script = "higher-1"
"#,
        );
        let merged = combine_files(lower, higher);
        let steps = merged.run.unwrap().prepare.unwrap().steps;
        assert_eq!(steps.len(), 1);
    }

    #[test]
    fn run_model_fallbacks_splice_inserts_inherited() {
        let lower = parse(
            r#"
[run.model]
fallbacks = ["openai", "gpt-5.4"]
"#,
        );
        let higher = parse(
            r#"
[run.model]
fallbacks = ["anthropic", "..."]
"#,
        );
        let merged = combine_files(lower, higher);
        let fallbacks = merged.run.unwrap().model.unwrap().fallbacks;
        // ["anthropic", "openai", "gpt-5.4"]
        assert_eq!(fallbacks.len(), 3);
    }

    #[test]
    fn hooks_replace_by_id_in_place() {
        let lower = parse(
            r#"
[[run.hooks]]
id = "shared"
event = "run_start"
script = "lower-script"
"#,
        );
        let higher = parse(
            r#"
[[run.hooks]]
id = "shared"
event = "run_start"
script = "higher-script"
"#,
        );
        let merged = combine_files(lower, higher);
        let hooks = merged.run.unwrap().hooks;
        assert_eq!(hooks.len(), 1);
        assert_eq!(
            hooks[0]
                .script
                .as_ref()
                .map(InterpString::as_source)
                .as_deref(),
            Some("higher-script")
        );
    }

    #[test]
    fn anonymous_hooks_append_after_merged_inherited() {
        let lower = parse(
            r#"
[[run.hooks]]
event = "run_start"
script = "lower-anon"
"#,
        );
        let higher = parse(
            r#"
[[run.hooks]]
event = "run_complete"
script = "higher-anon"
"#,
        );
        let merged = combine_files(lower, higher);
        let hooks = merged.run.unwrap().hooks;
        assert_eq!(hooks.len(), 2);
        assert_eq!(
            hooks[0]
                .script
                .as_ref()
                .map(InterpString::as_source)
                .as_deref(),
            Some("lower-anon")
        );
        assert_eq!(
            hooks[1]
                .script
                .as_ref()
                .map(InterpString::as_source)
                .as_deref(),
            Some("higher-anon")
        );
    }

    #[test]
    fn notification_route_events_splice() {
        let lower = parse(
            r#"
[run.notifications.ops]
events = ["run.failed"]
"#,
        );
        let higher = parse(
            r#"
[run.notifications.ops]
events = ["...", "run.completed"]
"#,
        );
        let merged = combine_files(lower, higher);
        let run = merged.run.unwrap();
        let events = &run.notifications["ops"].events;
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn project_metadata_replaces_wholesale() {
        let lower = parse(
            r#"
[project.metadata]
a = "1"
b = "2"
"#,
        );
        let higher = parse(
            r#"
[project.metadata]
a = "replaced"
"#,
        );
        let merged = combine_files(lower, higher);
        let meta = merged.project.unwrap().metadata;
        assert_eq!(meta.len(), 1);
        assert_eq!(meta.get("a"), Some(&"replaced".to_string()));
    }
}
