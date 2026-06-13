use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    ArtifactsSettings, GitAuthorSettings, HookDefinition, HookType, InterviewProviderSettings,
    McpServerSettings, McpTransport, MergeStrategy, NotificationProviderSettings,
    NotificationRouteSettings, PullRequestSettings, RunAgentSettings, RunBranchSettings,
    RunCheckpointSettings, RunCloneSettings, RunExecutionSettings, RunGitSettings, RunGoal,
    RunIntegrationsGithubSettings, RunIntegrationsSettings, RunInterviewsSettings,
    RunMetaBranchSettings, RunModelControls, RunModelSettings, RunNamespace, RunPrepareSettings,
    RunScmSettings, ScmGitHubSettings, TlsMode,
};

use super::{ResolveError, resolve_run_environment};
use crate::{
    EnvironmentLayer, HookAgentMarker, HookEntry, HookTlsMode, InterviewProviderLayer,
    InterviewsLayer, McpEntryLayer, MergeMap, ModelRefOrSplice, NotificationProviderLayer,
    NotificationRouteLayer, RunAgentLayer, RunArtifactsLayer, RunCheckpointLayer, RunCloneLayer,
    RunExecutionLayer, RunGitLayer, RunGoalLayer, RunIntegrationsLayer, RunLayer,
    RunMetaBranchLayer, RunModelLayer, RunPrepareLayer, RunPullRequestLayer, RunRunBranchLayer,
    RunScmLayer, StringOrSplice,
};

pub fn resolve_run(
    layer: &RunLayer,
    environments: &MergeMap<EnvironmentLayer>,
    errors: &mut Vec<ResolveError>,
) -> RunNamespace {
    let clone = resolve_clone(layer.clone.as_ref());
    let run_branch = resolve_run_branch(layer.run_branch.as_ref());
    let mut meta_branch = resolve_meta_branch(layer.meta_branch.as_ref());
    if !run_branch.enabled {
        if meta_branch.enabled || meta_branch.push {
            tracing::debug!(
                run_branch_enabled = run_branch.enabled,
                "Disabling metadata branch because run branch is disabled"
            );
        }
        meta_branch = RunMetaBranchSettings {
            enabled: false,
            push:    false,
        };
    }
    let pull_request = resolve_pull_request(layer.pull_request.as_ref());
    if pull_request.is_some() && (!run_branch.enabled || !run_branch.push) {
        errors.push(ResolveError::Invalid {
            path:   "run.pull_request".to_string(),
            reason: "run.pull_request.enabled requires run.run_branch.enabled and \
                     run.run_branch.push"
                .to_string(),
        });
    }

    RunNamespace {
        goal: resolve_goal(layer.goal.as_ref()),
        working_dir: layer.working_dir.clone(),
        metadata: layer.metadata.clone().into_inner(),
        inputs: layer.inputs.clone().unwrap_or_default(),
        model: resolve_model(layer.model.as_ref()),
        git: resolve_git(layer.git.as_ref()),
        prepare: resolve_prepare(layer.prepare.as_ref(), errors),
        execution: resolve_execution(layer.execution.as_ref()),
        checkpoint: resolve_checkpoint(layer.checkpoint.as_ref()),
        clone,
        run_branch,
        meta_branch,
        environment: resolve_run_environment(layer.environment.as_ref(), environments, errors),
        notifications: layer
            .notifications
            .iter()
            .map(|(name, route)| (name.clone(), resolve_notification_route(route)))
            .collect(),
        interviews: resolve_interviews(layer.interviews.as_ref()),
        agent: resolve_agent(layer.agent.as_ref()),
        hooks: layer
            .hooks
            .iter()
            .enumerate()
            .map(|(index, hook)| resolve_hook(hook, index, errors))
            .collect(),
        scm: resolve_scm(layer.scm.as_ref()),
        pull_request,
        artifacts: resolve_artifacts(layer.artifacts.as_ref()),
        integrations: resolve_integrations(layer.integrations.as_ref()),
    }
}

fn resolve_integrations(layer: Option<&RunIntegrationsLayer>) -> RunIntegrationsSettings {
    let github = layer
        .and_then(|integrations| integrations.github.as_ref())
        .map(|github| RunIntegrationsGithubSettings {
            // Collapse `Option<HashMap<...>>` -> `HashMap<...>`: both `None`
            // and `Some({})` resolve to an empty map (no token requested).
            // The presence distinction is only meaningful at merge time.
            permissions: github.permissions.clone().unwrap_or_default(),
        })
        .unwrap_or_default();
    RunIntegrationsSettings { github }
}

fn resolve_goal(goal: Option<&RunGoalLayer>) -> Option<RunGoal> {
    match goal? {
        RunGoalLayer::Inline(value) => Some(RunGoal::Inline(value.clone())),
        RunGoalLayer::File { file } => Some(RunGoal::File(file.clone())),
    }
}

fn resolve_model(model: Option<&RunModelLayer>) -> RunModelSettings {
    let Some(model) = model else {
        return RunModelSettings::default();
    };

    RunModelSettings {
        provider:  model.provider.clone(),
        name:      model.name.clone(),
        fallbacks: model
            .fallbacks
            .iter()
            .filter_map(|entry| match entry {
                ModelRefOrSplice::ModelRef(model_ref) => Some(model_ref.clone()),
                ModelRefOrSplice::Splice => None,
            })
            .collect(),
        controls:  model
            .controls
            .as_ref()
            .map(|c| RunModelControls {
                reasoning_effort: c.reasoning_effort.clone(),
                speed:            c.speed.clone(),
            })
            .unwrap_or_default(),
    }
}

fn resolve_git(git: Option<&RunGitLayer>) -> RunGitSettings {
    RunGitSettings {
        author: git.and_then(|git| {
            git.author.as_ref().map(|author| GitAuthorSettings {
                name:  author.name.clone(),
                email: author.email.clone(),
            })
        }),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "known leak: prepare step templates collapse to raw source unresolved; strict \
              resolution scheduled in the interpolation unification (Phase 2)"
)]
fn resolve_prepare(
    prepare: Option<&RunPrepareLayer>,
    errors: &mut Vec<ResolveError>,
) -> RunPrepareSettings {
    let prepare = prepare.expect("defaults.toml should provide run.prepare defaults");

    let mut commands = Vec::new();
    for (index, step) in prepare.steps.iter().enumerate() {
        match (&step.script, &step.command) {
            (Some(script), None) => commands.push(script.as_source()),
            (None, Some(argv)) => commands.push(
                argv.iter()
                    .map(InterpString::as_source)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            (Some(_), Some(_)) | (None, None) => errors.push(ResolveError::Invalid {
                path:   format!("run.prepare.steps[{index}]"),
                reason: "exactly one of script or command must be set".to_string(),
            }),
        }
    }

    RunPrepareSettings {
        commands,
        timeout_ms: prepare.timeout.map_or(300_000, |timeout| {
            u64::try_from(timeout.as_std().as_millis()).unwrap_or(u64::MAX)
        }),
    }
}

fn resolve_execution(execution: Option<&RunExecutionLayer>) -> RunExecutionSettings {
    let execution = execution.expect("defaults.toml should provide run.execution defaults");

    RunExecutionSettings {
        mode:     execution
            .mode
            .expect("defaults.toml should provide run.execution.mode"),
        approval: execution
            .approval
            .expect("defaults.toml should provide run.execution.approval"),
    }
}

fn resolve_checkpoint(checkpoint: Option<&RunCheckpointLayer>) -> RunCheckpointSettings {
    RunCheckpointSettings {
        exclude_globs:  checkpoint
            .map(|checkpoint| checkpoint.exclude_globs.clone())
            .unwrap_or_default(),
        skip_git_hooks: checkpoint
            .and_then(|checkpoint| checkpoint.skip_git_hooks)
            .unwrap_or(false),
    }
}

fn resolve_clone(clone: Option<&RunCloneLayer>) -> RunCloneSettings {
    RunCloneSettings {
        enabled: clone.and_then(|clone| clone.enabled).unwrap_or(true),
    }
}

fn resolve_run_branch(run_branch: Option<&RunRunBranchLayer>) -> RunBranchSettings {
    RunBranchSettings {
        enabled: run_branch
            .and_then(|run_branch| run_branch.enabled)
            .unwrap_or(true),
        push:    run_branch
            .and_then(|run_branch| run_branch.push)
            .unwrap_or(true),
    }
}

fn resolve_meta_branch(meta_branch: Option<&RunMetaBranchLayer>) -> RunMetaBranchSettings {
    RunMetaBranchSettings {
        enabled: meta_branch
            .and_then(|meta_branch| meta_branch.enabled)
            .unwrap_or(true),
        push:    meta_branch
            .and_then(|meta_branch| meta_branch.push)
            .unwrap_or(true),
    }
}

fn resolve_notification_route(route: &NotificationRouteLayer) -> NotificationRouteSettings {
    NotificationRouteSettings {
        enabled:  route.enabled.unwrap_or(false),
        provider: route.provider.clone(),
        events:   route
            .events
            .iter()
            .filter_map(|event| match event {
                StringOrSplice::Value(value) => Some(value.clone()),
                StringOrSplice::Splice => None,
            })
            .collect(),
        slack:    route.slack.as_ref().map(resolve_notification_provider),
    }
}

fn resolve_notification_provider(
    provider: &NotificationProviderLayer,
) -> NotificationProviderSettings {
    NotificationProviderSettings {
        channel: provider.channel.clone(),
    }
}

fn resolve_interviews(interviews: Option<&InterviewsLayer>) -> RunInterviewsSettings {
    let Some(interviews) = interviews else {
        return RunInterviewsSettings::default();
    };

    RunInterviewsSettings {
        provider: interviews.provider.clone(),
        slack:    interviews.slack.as_ref().map(resolve_interview_provider),
    }
}

fn resolve_interview_provider(provider: &InterviewProviderLayer) -> InterviewProviderSettings {
    InterviewProviderSettings {
        channel: provider.channel.clone(),
    }
}

fn resolve_agent(agent: Option<&RunAgentLayer>) -> RunAgentSettings {
    let Some(agent) = agent else {
        return RunAgentSettings::default();
    };

    RunAgentSettings {
        fabro_tools: agent.fabro_tools.unwrap_or(false),
        permissions: agent.permissions,
        mcps:        agent
            .mcps
            .iter()
            .map(|(name, entry)| (name.clone(), resolve_mcp_entry(name, entry)))
            .collect(),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "known leak: MCP transport templates collapse to raw source unresolved; strict \
              resolution scheduled in the interpolation unification (Phase 2, supersedes PR #370)"
)]
pub(crate) fn resolve_mcp_entry(name: &str, entry: &McpEntryLayer) -> McpServerSettings {
    let transport = match entry {
        McpEntryLayer::Stdio {
            script,
            command,
            env,
            ..
        } => McpTransport::Stdio {
            command: resolve_mcp_command(script.as_ref(), command.as_ref()),
            env:     env
                .iter()
                .map(|(key, value)| (key.clone(), value.as_source()))
                .collect(),
        },
        McpEntryLayer::Http {
            protocol,
            url,
            headers,
            ..
        } => McpTransport::Http {
            protocol: *protocol,
            url:      url.as_source(),
            headers:  headers
                .iter()
                .map(|(key, value)| (key.clone(), value.as_source()))
                .collect(),
        },
        McpEntryLayer::Sandbox {
            protocol,
            script,
            command,
            port,
            env,
            ..
        } => McpTransport::Sandbox {
            protocol: *protocol,
            command:  resolve_mcp_command(script.as_ref(), command.as_ref()),
            port:     *port,
            env:      env
                .iter()
                .map(|(key, value)| (key.clone(), value.as_source()))
                .collect(),
        },
    };

    let (startup_timeout_secs, tool_timeout_secs) = match entry {
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
            startup_timeout.map_or(10, |timeout| timeout.as_std().as_secs()),
            tool_timeout.map_or(60, |timeout| timeout.as_std().as_secs()),
        ),
    };

    McpServerSettings {
        name: name.to_string(),
        transport,
        current_dir: None,
        clear_env: false,
        startup_timeout_secs,
        tool_timeout_secs,
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "known leak: MCP transport templates collapse to raw source unresolved; strict \
              resolution scheduled in the interpolation unification (Phase 2, supersedes PR #370)"
)]
fn resolve_mcp_command(
    script: Option<&InterpString>,
    command: Option<&Vec<InterpString>>,
) -> Vec<String> {
    if let Some(script) = script {
        return vec!["sh".to_string(), "-c".to_string(), script.as_source()];
    }
    command
        .map(|command| command.iter().map(InterpString::as_source).collect())
        .unwrap_or_default()
}

#[expect(
    clippy::disallowed_methods,
    reason = "intentional source preservation: the hook executor re-resolves {{ env.* }} \
              tokens at hook fire time"
)]
fn resolve_hook(hook: &HookEntry, index: usize, errors: &mut Vec<ResolveError>) -> HookDefinition {
    let variants = [
        hook.script.is_some() || hook.command.is_some(),
        hook.url.is_some(),
        hook.prompt.is_some() && hook.agent.is_none(),
        hook.agent == Some(HookAgentMarker::Enabled),
    ]
    .into_iter()
    .filter(|flag| *flag)
    .count();

    if variants != 1 {
        errors.push(ResolveError::Invalid {
            path:   format!("run.hooks[{index}]"),
            reason: "exactly one hook transport must be configured".to_string(),
        });
    }

    let hook_type = resolve_hook_type(hook);
    let command = if let Some(script) = &hook.script {
        Some(script.as_source())
    } else {
        hook.command.as_ref().map(|command| {
            command
                .iter()
                .map(InterpString::as_source)
                .collect::<Vec<_>>()
                .join(" ")
        })
    };

    HookDefinition {
        name: hook.name.clone().or_else(|| hook.id.clone()),
        event: hook.event,
        command,
        hook_type,
        matcher: hook.matcher.clone(),
        blocking: hook.blocking,
        timeout_ms: hook
            .timeout
            .map(|timeout| u64::try_from(timeout.as_std().as_millis()).unwrap_or(u64::MAX)),
        sandbox: hook.sandbox,
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "intentional source preservation: the hook executor re-resolves {{ env.* }} \
              tokens at hook fire time"
)]
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
                    .map(|(key, value)| (key.clone(), value.as_source()))
                    .collect(),
            )
        };
        let tls = match hook.tls {
            Some(HookTlsMode::Verify) => TlsMode::Verify,
            Some(HookTlsMode::NoVerify) => TlsMode::NoVerify,
            Some(HookTlsMode::Off) => TlsMode::Off,
            None => TlsMode::default(),
        };
        return Some(HookType::Http {
            url: url.as_source(),
            headers,
            allowed_env_vars: hook.allowed_env_vars.clone(),
            tls,
        });
    }

    if hook.agent == Some(HookAgentMarker::Enabled) {
        return Some(HookType::Agent {
            prompt:          hook
                .prompt
                .as_ref()
                .map(InterpString::as_source)
                .unwrap_or_default(),
            model:           hook.model.as_ref().map(InterpString::as_source),
            max_tool_rounds: hook.max_tool_rounds,
        });
    }

    hook.prompt.as_ref().map(|prompt| HookType::Prompt {
        prompt: prompt.as_source(),
        model:  hook.model.as_ref().map(InterpString::as_source),
    })
}

fn resolve_scm(scm: Option<&RunScmLayer>) -> RunScmSettings {
    let Some(scm) = scm else {
        return RunScmSettings::default();
    };

    RunScmSettings {
        provider:   scm.provider.clone(),
        owner:      scm.owner.clone(),
        repository: scm.repository.clone(),
        github:     scm.github.as_ref().map(|_| ScmGitHubSettings {}),
    }
}

fn resolve_pull_request(pull_request: Option<&RunPullRequestLayer>) -> Option<PullRequestSettings> {
    let pull_request = pull_request?;
    if !pull_request.enabled.unwrap_or(false) {
        return None;
    }

    Some(PullRequestSettings {
        enabled:        true,
        draft:          pull_request.draft.unwrap_or(true),
        auto_merge:     pull_request.auto_merge.unwrap_or(false),
        merge_strategy: pull_request.merge_strategy.unwrap_or(MergeStrategy::Squash),
    })
}

fn resolve_artifacts(artifacts: Option<&RunArtifactsLayer>) -> ArtifactsSettings {
    ArtifactsSettings {
        include: artifacts
            .map(|artifacts| artifacts.include.clone())
            .unwrap_or_default(),
    }
}
