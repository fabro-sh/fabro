use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    ApprovalMode, ArtifactsSettings, DaytonaDockerfileLayer, DaytonaSandboxLayer, DaytonaSettings,
    DaytonaSnapshotSettings, DockerfileSource, GitAuthorSettings, HookAgentMarker, HookDefinition,
    HookEntry, HookTlsMode, HookType, InterviewProviderLayer, InterviewProviderSettings,
    InterviewsLayer, LocalSandboxSettings, McpEntryLayer, McpServerSettings, McpTransport,
    MergeStrategy, ModelRefOrSplice, NotificationProviderLayer, NotificationProviderSettings,
    NotificationRouteLayer, NotificationRouteSettings, PullRequestSettings, RunAgentLayer,
    RunAgentSettings, RunArtifactsLayer, RunCheckpointLayer, RunCheckpointSettings,
    RunExecutionLayer, RunExecutionSettings, RunGitLayer, RunGitSettings, RunGoal, RunGoalLayer,
    RunInterviewsSettings, RunLayer, RunMode, RunModelLayer, RunModelSettings, RunPrepareLayer,
    RunPrepareSettings, RunPullRequestLayer, RunSandboxLayer, RunSandboxSettings, RunScmLayer,
    RunScmSettings, RunSettings, ScmGitHubSettings, StringOrSplice, TlsMode,
};

use super::ResolveError;

pub fn resolve_run(layer: &RunLayer, errors: &mut Vec<ResolveError>) -> RunSettings {
    RunSettings {
        goal:          resolve_goal(layer.goal.as_ref()),
        working_dir:   layer.working_dir.clone(),
        metadata:      layer.metadata.clone(),
        inputs:        layer.inputs.clone().unwrap_or_default(),
        model:         resolve_model(layer.model.as_ref()),
        git:           resolve_git(layer.git.as_ref()),
        prepare:       resolve_prepare(layer.prepare.as_ref(), errors),
        execution:     resolve_execution(layer.execution.as_ref()),
        checkpoint:    resolve_checkpoint(layer.checkpoint.as_ref()),
        sandbox:       resolve_sandbox(layer.sandbox.as_ref(), errors),
        notifications: layer
            .notifications
            .iter()
            .map(|(name, route)| (name.clone(), resolve_notification_route(route)))
            .collect(),
        interviews:    resolve_interviews(layer.interviews.as_ref()),
        agent:         resolve_agent(layer.agent.as_ref()),
        hooks:         layer
            .hooks
            .iter()
            .enumerate()
            .map(|(index, hook)| resolve_hook(hook, index, errors))
            .collect(),
        scm:           resolve_scm(layer.scm.as_ref()),
        pull_request:  resolve_pull_request(layer.pull_request.as_ref()),
        artifacts:     resolve_artifacts(layer.artifacts.as_ref()),
    }
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

fn resolve_prepare(
    prepare: Option<&RunPrepareLayer>,
    errors: &mut Vec<ResolveError>,
) -> RunPrepareSettings {
    let Some(prepare) = prepare else {
        return RunPrepareSettings::default();
    };

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
    let Some(execution) = execution else {
        return RunExecutionSettings::default();
    };

    RunExecutionSettings {
        mode:     execution.mode.unwrap_or(RunMode::Normal),
        approval: execution.approval.unwrap_or(ApprovalMode::Prompt),
        retros:   execution.retros.unwrap_or(true),
    }
}

fn resolve_checkpoint(checkpoint: Option<&RunCheckpointLayer>) -> RunCheckpointSettings {
    RunCheckpointSettings {
        exclude_globs: checkpoint
            .map(|checkpoint| checkpoint.exclude_globs.clone())
            .unwrap_or_default(),
    }
}

fn resolve_sandbox(
    sandbox: Option<&RunSandboxLayer>,
    errors: &mut Vec<ResolveError>,
) -> RunSandboxSettings {
    let Some(sandbox) = sandbox else {
        return RunSandboxSettings::default();
    };

    let provider = sandbox
        .provider
        .clone()
        .unwrap_or_else(|| "local".to_string());
    match provider.as_str() {
        "local" | "docker" | "daytona" => {}
        other => errors.push(ResolveError::Invalid {
            path:   "run.sandbox.provider".to_string(),
            reason: format!("unknown sandbox provider: {other}"),
        }),
    }

    RunSandboxSettings {
        provider,
        preserve: sandbox.preserve.unwrap_or(false),
        devcontainer: sandbox.devcontainer.unwrap_or(false),
        env: sandbox.env.clone(),
        local: resolve_local_sandbox(sandbox),
        daytona: sandbox.daytona.as_ref().map(resolve_daytona),
    }
}

fn resolve_local_sandbox(sandbox: &RunSandboxLayer) -> LocalSandboxSettings {
    LocalSandboxSettings {
        worktree_mode: sandbox
            .local
            .as_ref()
            .and_then(|local| local.worktree_mode)
            .unwrap_or_default(),
    }
}

fn resolve_daytona(daytona: &DaytonaSandboxLayer) -> DaytonaSettings {
    DaytonaSettings {
        auto_stop_interval: daytona.auto_stop_interval,
        labels:             daytona.labels.clone(),
        snapshot:           daytona.snapshot.as_ref().and_then(|snapshot| {
            snapshot.name.as_ref().map(|name| DaytonaSnapshotSettings {
                name:       name.clone(),
                cpu:        snapshot.cpu,
                memory_gb:  snapshot.memory.map(|size| size_to_gb_i32(size.as_bytes())),
                disk_gb:    snapshot.disk.map(|size| size_to_gb_i32(size.as_bytes())),
                dockerfile: snapshot
                    .dockerfile
                    .as_ref()
                    .map(|dockerfile| match dockerfile {
                        DaytonaDockerfileLayer::Inline(text) => {
                            DockerfileSource::Inline(text.clone())
                        }
                        DaytonaDockerfileLayer::Path { path } => {
                            DockerfileSource::Path { path: path.clone() }
                        }
                    }),
            })
        }),
        network:            daytona.network.clone(),
        skip_clone:         daytona.skip_clone.unwrap_or(false),
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
        discord:  route.discord.as_ref().map(resolve_notification_provider),
        teams:    route.teams.as_ref().map(resolve_notification_provider),
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
        discord:  interviews.discord.as_ref().map(resolve_interview_provider),
        teams:    interviews.teams.as_ref().map(resolve_interview_provider),
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
        permissions: agent.permissions,
        mcps:        agent
            .mcps
            .iter()
            .map(|(name, entry)| (name.clone(), resolve_mcp_entry(name, entry)))
            .collect(),
    }
}

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
        McpEntryLayer::Http { url, headers, .. } => McpTransport::Http {
            url:     url.as_source(),
            headers: headers
                .iter()
                .map(|(key, value)| (key.clone(), value.as_source()))
                .collect(),
        },
        McpEntryLayer::Sandbox {
            script,
            command,
            port,
            env,
            ..
        } => McpTransport::Sandbox {
            command: resolve_mcp_command(script.as_ref(), command.as_ref()),
            port:    *port,
            env:     env
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
        startup_timeout_secs,
        tool_timeout_secs,
    }
}

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
        github:     scm.github.as_ref().map(|_| ScmGitHubSettings),
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

fn size_to_gb_i32(bytes: u64) -> i32 {
    let gb = bytes / 1_000_000_000;
    i32::try_from(gb).unwrap_or(i32::MAX)
}
