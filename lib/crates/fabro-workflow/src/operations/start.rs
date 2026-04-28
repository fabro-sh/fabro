use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fabro_auth::configured_providers_from_process_env;
use fabro_config::project as project_config;
use fabro_interview::{AutoApproveInterviewer, Interviewer};
use fabro_mcp::config::{McpServerSettings, McpTransport};
use fabro_model::{Catalog, FallbackTarget, Provider};
use fabro_retro::retro::Retro;
use fabro_sandbox::config::{
    self as sandbox_config, DaytonaNetwork, DaytonaSnapshotSettings,
    DockerfileSource as SandboxDockerfileSource, WorktreeMode, bridge_worktree_mode,
};
use fabro_sandbox::daytona::DaytonaConfig;
use fabro_sandbox::{DockerSandboxOptions, SandboxProvider, SandboxSpec};
use fabro_static::EnvVars;
use fabro_types::RunId;
use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    ApprovalMode, DaytonaNetworkLayer, DaytonaSettings, DockerSettings,
    DockerfileSource as ResolvedDockerfileSource, HookDefinition as ResolvedHookDefinition,
    HookEvent as ResolvedHookEvent, HookType as ResolvedHookType,
    McpServerSettings as ResolvedMcpServerSettings, McpTransport as ResolvedMcpTransport,
    PullRequestSettings, RunMode, RunModelSettings as ResolvedRunModelSettings,
    RunNamespace as ResolvedRunSettings, TlsMode as ResolvedTlsMode,
};
use fabro_vault::Vault;
use tokio::runtime::Handle;
use tokio::sync::RwLock as AsyncRwLock;

use crate::artifact_upload::ArtifactSink;
use crate::context::Context;
use crate::error::Error;
use crate::event::{
    Emitter, Event, EventBody, RunEventLogger, RunEventSink, RunNoticeLevel, append_event_to_sink,
};
use crate::handler::HandlerRegistry;
use crate::outcome::{Outcome, StageStatus};
use crate::pipeline::{
    self, DevcontainerSpec, FinalizeOptions, Finalized, InitOptions, LlmSpec, Persisted,
    PullRequestOptions, RetroOptions, SandboxEnvSpec, build_conclusion_from_store,
    classify_engine_result,
};
use crate::records::Checkpoint;
use crate::run_control::RunControlState;
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions};
use crate::run_status::{FailureReason, RunStatus};
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_metadata::metadata_branch_name;
use crate::workflow_bundle::{RunDefinition, WorkflowBundle};

struct RunSession {
    cancel_token:      Option<Arc<AtomicBool>>,
    emitter:           Arc<Emitter>,
    sandbox:           SandboxSpec,
    llm:               LlmSpec,
    interviewer:       Arc<dyn Interviewer>,
    on_node:           crate::OnNodeCallback,
    lifecycle:         LifecycleOptions,
    hooks:             fabro_hooks::HookSettings,
    sandbox_env:       SandboxEnvSpec,
    devcontainer:      Option<DevcontainerSpec>,
    seed_context:      Option<Context>,
    run_store:         RunStoreHandle,
    event_sink:        RunEventSink,
    artifact_sink:     Option<ArtifactSink>,
    git:               Option<GitCheckpointOptions>,
    github_app:        Option<fabro_github::GitHubCredentials>,
    worktree_mode:     Option<WorktreeMode>,
    registry_override: Option<Arc<HandlerRegistry>>,
    retro_enabled:     bool,
    preserve_sandbox:  bool,
    pr_config:         Option<PullRequestSettings>,
    pr_github_app:     Option<fabro_github::GitHubCredentials>,
    pr_origin_url:     Option<String>,
    pr_model:          String,
    workflow_path:     Option<PathBuf>,
    workflow_bundle:   Option<Arc<WorkflowBundle>>,
    run_control:       Option<Arc<RunControlState>>,
    vault:             Option<Arc<AsyncRwLock<Vault>>>,
}

pub struct StartServices {
    pub run_id:             RunId,
    pub cancel_token:       Option<Arc<AtomicBool>>,
    pub emitter:            Arc<Emitter>,
    pub interviewer:        Arc<dyn Interviewer>,
    pub run_store:          RunStoreHandle,
    pub event_sink:         RunEventSink,
    pub artifact_sink:      Option<ArtifactSink>,
    pub run_control:        Option<Arc<RunControlState>>,
    pub github_app:         Option<fabro_github::GitHubCredentials>,
    /// Server-resolved GitHub integration permissions to inject into the
    /// sandbox env. Empty when github integration has no permissions.
    pub github_permissions: HashMap<String, String>,
    pub vault:              Option<Arc<AsyncRwLock<Vault>>>,
    pub on_node:            crate::OnNodeCallback,
    pub registry_override:  Option<Arc<HandlerRegistry>>,
}

pub struct Started {
    pub finalized:      Finalized,
    pub final_context:  Option<Context>,
    pub retro:          Option<Retro>,
    pub retro_duration: Duration,
}

/// Start a fresh workflow run. Errors if a checkpoint already exists (use
/// `resume()` instead).
pub async fn start(run_dir: &Path, services: StartServices) -> Result<Started, Error> {
    std::fs::create_dir_all(run_dir).map_err(|err| {
        Error::Io(format!(
            "creating run directory {}: {err}",
            run_dir.display()
        ))
    })?;
    let state = services
        .run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    if state.checkpoint.is_some() {
        return Err(Error::Precondition(
            "checkpoint already exists in the run store — did you mean to resume?".to_string(),
        ));
    }

    if let Some(status) = state.status {
        if !matches!(
            status,
            RunStatus::Submitted | RunStatus::Queued | RunStatus::Starting
        ) {
            return Err(Error::Precondition(format!(
                "cannot start run: status is {status}, expected submitted"
            )));
        }
    }

    Box::pin(execute_persisted_run(run_dir, None, services)).await
}

pub(super) async fn execute_persisted_run(
    run_dir: &Path,
    checkpoint: Option<Checkpoint>,
    services: StartServices,
) -> Result<Started, Error> {
    let cancel_token = services.cancel_token.clone();
    let run_id = services.run_id;
    let run_store = services.run_store.clone();
    let event_sink = services.event_sink.clone();
    if let Err(err) = run_store.state().await {
        let error = Error::engine(err.to_string());
        let _ = persist_detached_failure(
            run_id,
            &event_sink,
            run_dir,
            "bootstrap",
            FailureReason::BootstrapFailed,
            &error,
        )
        .await;
        return Err(error);
    }
    if let Err(err) = append_event_to_sink(&event_sink, &run_id, &Event::RunStarting).await {
        let error = Error::engine(err.to_string());
        let _ = persist_detached_failure(
            run_id,
            &event_sink,
            run_dir,
            "bootstrap",
            FailureReason::BootstrapFailed,
            &error,
        )
        .await;
        return Err(error);
    }

    let mut bootstrap_guard =
        DetachedRunBootstrapGuard::arm(run_id, run_dir, event_sink.clone(), cancel_token.clone());

    let persisted = match Persisted::load_from_store(&services.run_store, run_dir).await {
        Ok(persisted) => persisted,
        Err(err) => {
            let _ = persist_detached_failure(
                run_id,
                &event_sink,
                run_dir,
                "bootstrap",
                FailureReason::BootstrapFailed,
                &err,
            )
            .await;
            bootstrap_guard.defuse();
            return Err(err);
        }
    };

    let session = match RunSession::new(&persisted, services).await {
        Ok(session) => session,
        Err(err) => {
            let _ = persist_detached_failure(
                run_id,
                &event_sink,
                run_dir,
                "bootstrap",
                FailureReason::BootstrapFailed,
                &err,
            )
            .await;
            bootstrap_guard.defuse();
            return Err(err);
        }
    };

    bootstrap_guard.defuse();
    let mut completion_guard =
        DetachedRunCompletionGuard::arm(run_id, event_sink.clone(), cancel_token);
    let run_start = Instant::now();
    let started = Box::pin(session.run(persisted, checkpoint)).await;

    match started {
        Ok(started) => {
            completion_guard.defuse();
            Ok(started)
        }
        Err(err) => {
            persist_terminal_engine_failure(
                run_id,
                &run_store,
                &event_sink,
                run_dir,
                &err,
                run_start.elapsed(),
            )
            .await;
            completion_guard.defuse();
            Err(err)
        }
    }
}

async fn persist_terminal_engine_failure(
    run_id: RunId,
    run_store: &RunStoreHandle,
    event_sink: &RunEventSink,
    _run_dir: &Path,
    error: &Error,
    duration: Duration,
) {
    let engine_result: Result<Outcome, Error> = Err(error.clone());
    let (final_status, failure_reason, run_status) = classify_engine_result(&engine_result);
    let _conclusion = build_conclusion_from_store(
        run_store,
        final_status,
        failure_reason,
        crate::millis_u64(duration),
        None,
    )
    .await;
    let reason = match run_status {
        RunStatus::Failed { reason } => reason,
        _ => FailureReason::WorkflowError,
    };
    if let Err(err) = append_event_to_sink(event_sink, &run_id, &Event::WorkflowRunFailed {
        error: error.clone(),
        duration_ms: crate::millis_u64(duration),
        reason,
        git_commit_sha: None,
        final_patch: None,
    })
    .await
    {
        tracing::warn!(error = %err, "Failed to append terminal engine failure event");
    }
}

impl RunSession {
    async fn new(persisted: &Persisted, services: StartServices) -> Result<Self, Error> {
        let record = persisted.run_spec();
        let settings = &record.settings;
        let working_directory = record
            .source_directory
            .as_deref()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);
        let state = services
            .run_store
            .state()
            .await
            .map_err(|err| Error::engine(err.to_string()))?;
        let git = state.start.and_then(|start| {
            start.run_branch.as_ref().map(|_| GitCheckpointOptions {
                base_sha:    start.base_sha.clone(),
                run_branch:  start.run_branch.clone(),
                meta_branch: Some(metadata_branch_name(&record.run_id.to_string())),
            })
        });
        let definition_blob = state.spec.as_ref().and_then(|run| run.definition_blob);
        let accepted_definition = match definition_blob {
            Some(blob_id) => {
                Some(load_accepted_run_definition(&services.run_store, blob_id).await?)
            }
            None => None,
        };
        let workflow_path = accepted_definition
            .as_ref()
            .map(|definition| definition.workflow_path.clone());
        let workflow_bundle =
            accepted_definition.map(|definition| Arc::new(definition.workflow_bundle()));

        let resolved = &settings.run;

        let sandbox_provider = resolve_sandbox_provider(resolved)?;
        let sandbox_provider =
            if resolved.execution.mode == RunMode::DryRun && !sandbox_provider.is_local() {
                SandboxProvider::Local
            } else {
                sandbox_provider
            };
        let configured = configured_providers_from_process_env(services.vault.as_ref()).await;
        let model = resolved.model.name.as_ref().map_or_else(
            || {
                Catalog::builtin()
                    .default_for_configured(&configured)
                    .id
                    .clone()
            },
            InterpString::as_source,
        );
        let provider = resolved
            .model
            .provider
            .as_ref()
            .map(InterpString::as_source)
            .filter(|value| !value.is_empty());

        let provider_enum: Provider = match provider.as_deref() {
            Some(value) => value
                .parse::<Provider>()
                .map_err(|_| Error::Precondition(format!("unknown provider: {value}")))?,
            None => Provider::default_for_configured(&configured),
        };

        let fallback_chain = resolve_fallback_chain(provider_enum, &model, &resolved.model);
        let mcp_servers = resolved
            .agent
            .mcps
            .values()
            .map(runtime_mcp_server)
            .collect();

        let sandbox = match sandbox_provider {
            SandboxProvider::Local => SandboxSpec::Local {
                working_directory: working_directory.clone(),
            },
            SandboxProvider::Docker => SandboxSpec::Docker {
                config:           resolve_docker_config(resolved).unwrap_or_default(),
                github_app:       services.github_app.clone(),
                run_id:           Some(record.run_id),
                clone_origin_url: record.repo_origin_url().map(str::to_string),
                clone_branch:     record.base_branch().map(str::to_string),
            },
            SandboxProvider::Daytona => {
                let api_key = match &services.vault {
                    Some(v) => v
                        .read()
                        .await
                        .get(EnvVars::DAYTONA_API_KEY)
                        .map(str::to_string),
                    None => None,
                };
                SandboxSpec::Daytona {
                    config: resolve_daytona_config(resolved).unwrap_or_default(),
                    github_app: services.github_app.clone(),
                    run_id: Some(record.run_id),
                    clone_origin_url: record.repo_origin_url().map(str::to_string),
                    clone_branch: record.base_branch().map(str::to_string),
                    api_key,
                }
            }
        };

        let toml_env: HashMap<String, String> = resolved
            .sandbox
            .env
            .iter()
            .map(|(k, v)| (k.clone(), resolve_interp(v)))
            .collect();
        let github_permissions: Option<HashMap<String, String>> =
            (!services.github_permissions.is_empty()).then(|| services.github_permissions.clone());
        let sandbox_env = SandboxEnvSpec {
            devcontainer_env: HashMap::new(),
            toml_env,
            github_permissions,
            origin_url: record.repo_origin_url().map(str::to_string),
        };

        let devcontainer = resolved.sandbox.devcontainer.then(|| DevcontainerSpec {
            enabled:     true,
            resolve_dir: working_directory.clone(),
        });

        let interviewer: Arc<dyn Interviewer> = if resolved.execution.approval == ApprovalMode::Auto
        {
            Arc::new(AutoApproveInterviewer)
        } else {
            services.interviewer
        };

        let pr_config = resolved.pull_request.clone();

        Ok(Self {
            cancel_token: services.cancel_token,
            emitter: services.emitter,
            event_sink: services.event_sink,
            run_control: services.run_control,
            sandbox,
            llm: LlmSpec {
                model: model.clone(),
                provider: provider_enum,
                fallback_chain,
                mcp_servers,
                dry_run: resolved.execution.mode == RunMode::DryRun,
            },
            interviewer,
            on_node: services.on_node,
            lifecycle: LifecycleOptions {
                setup_commands:           resolved.prepare.commands.clone(),
                setup_command_timeout_ms: resolved.prepare.timeout_ms,
                devcontainer_phases:      Vec::new(),
            },
            hooks: fabro_hooks::HookSettings {
                hooks: resolved.hooks.iter().map(runtime_hook_definition).collect(),
            },
            sandbox_env,
            devcontainer,
            seed_context: None,
            run_store: services.run_store,
            artifact_sink: services.artifact_sink,
            git,
            github_app: services.github_app.clone(),
            worktree_mode: Some(resolve_worktree_mode(resolved)),
            registry_override: services.registry_override,
            retro_enabled: resolved.execution.retros && project_config::is_retro_enabled(),
            preserve_sandbox: resolved.sandbox.preserve,
            pr_config,
            pr_github_app: services.github_app,
            pr_origin_url: record.repo_origin_url().map(str::to_string),
            pr_model: model,
            workflow_path,
            workflow_bundle,
            vault: services.vault,
        })
    }
}

fn resolve_interp(value: &InterpString) -> String {
    value
        .resolve(process_env_var)
        .map_or_else(|_| value.as_source(), |resolved| resolved.value)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Run startup interpolation owns a process-env lookup facade for {{ env.* }} values."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

async fn load_accepted_run_definition(
    run_store: &RunStoreHandle,
    blob_id: fabro_types::RunBlobId,
) -> Result<RunDefinition, Error> {
    let bytes = run_store
        .read_blob(&blob_id)
        .await
        .map_err(|err| Error::engine(err.to_string()))?
        .ok_or_else(|| {
            Error::engine(format!(
                "run definition blob is missing from the run store: {blob_id}"
            ))
        })?;
    serde_json::from_slice(&bytes).map_err(|err| Error::Parse(err.to_string()))
}

fn resolve_sandbox_provider(settings: &ResolvedRunSettings) -> Result<SandboxProvider, Error> {
    Some(str::parse::<SandboxProvider>(
        settings.sandbox.provider.as_str(),
    ))
    .transpose()
    .map_err(|err| Error::Precondition(format!("Invalid sandbox provider: {err}")))?
    .map_or_else(|| Ok(SandboxProvider::default()), Ok)
}

fn resolve_worktree_mode(settings: &ResolvedRunSettings) -> sandbox_config::WorktreeMode {
    bridge_worktree_mode(settings.sandbox.local.worktree_mode)
}

fn resolve_daytona_config(settings: &ResolvedRunSettings) -> Option<DaytonaConfig> {
    settings
        .sandbox
        .daytona
        .as_ref()
        .map(runtime_daytona_config)
}

fn resolve_docker_config(settings: &ResolvedRunSettings) -> Option<DockerSandboxOptions> {
    settings.sandbox.docker.as_ref().map(runtime_docker_config)
}

fn resolve_fallback_chain(
    provider: Provider,
    model: &str,
    settings: &ResolvedRunModelSettings,
) -> Vec<FallbackTarget> {
    if settings.fallbacks.is_empty() {
        return Vec::new();
    }
    // Group v2 ModelRef entries by provider name, preserving the legacy
    // shape expected by `Catalog::build_fallback_chain`. The historical
    // bridge grouped all fallback tokens under the empty-string key; we
    // preserve that behavior here so `Catalog::build_fallback_chain`
    // returns an empty chain unless a consumer has explicitly wired
    // provider-keyed fallbacks. A proper provider-aware fallback chain
    // is a follow-up along with the model registry work.
    let mut by_provider: HashMap<String, Vec<String>> = HashMap::new();
    for model_ref in &settings.fallbacks {
        by_provider
            .entry(String::new())
            .or_default()
            .push(model_ref.to_string());
    }
    Catalog::builtin().build_fallback_chain(provider, model, &by_provider)
}

fn runtime_mcp_server(settings: &ResolvedMcpServerSettings) -> McpServerSettings {
    McpServerSettings {
        name:                 settings.name.clone(),
        transport:            match &settings.transport {
            ResolvedMcpTransport::Stdio { command, env } => McpTransport::Stdio {
                command: command.clone(),
                env:     env.clone(),
            },
            ResolvedMcpTransport::Http { url, headers } => McpTransport::Http {
                url:     url.clone(),
                headers: headers.clone(),
            },
            ResolvedMcpTransport::Sandbox { command, port, env } => McpTransport::Sandbox {
                command: command.clone(),
                port:    *port,
                env:     env.clone(),
            },
        },
        startup_timeout_secs: settings.startup_timeout_secs,
        tool_timeout_secs:    settings.tool_timeout_secs,
    }
}

fn runtime_daytona_config(settings: &DaytonaSettings) -> DaytonaConfig {
    DaytonaConfig {
        auto_stop_interval: settings.auto_stop_interval,
        labels:             (!settings.labels.is_empty()).then_some(settings.labels.clone()),
        snapshot:           settings
            .snapshot
            .as_ref()
            .map(|snapshot| DaytonaSnapshotSettings {
                name:       snapshot.name.clone(),
                cpu:        snapshot.cpu,
                memory:     snapshot.memory_gb,
                disk:       snapshot.disk_gb,
                dockerfile: snapshot
                    .dockerfile
                    .as_ref()
                    .map(|dockerfile| match dockerfile {
                        ResolvedDockerfileSource::Inline(text) => {
                            SandboxDockerfileSource::Inline(text.clone())
                        }
                        ResolvedDockerfileSource::Path { path } => {
                            SandboxDockerfileSource::Path { path: path.clone() }
                        }
                    }),
            }),
        network:            settings.network.as_ref().map(|network| match network {
            DaytonaNetworkLayer::Block => DaytonaNetwork::Block,
            DaytonaNetworkLayer::AllowAll => DaytonaNetwork::AllowAll,
            DaytonaNetworkLayer::AllowList { allow_list } => {
                DaytonaNetwork::AllowList(allow_list.clone())
            }
        }),
        skip_clone:         settings.skip_clone,
    }
}

fn runtime_docker_config(settings: &DockerSettings) -> DockerSandboxOptions {
    let mut env_vars = settings
        .env_vars
        .iter()
        .map(|(key, value)| format!("{key}={}", resolve_interp(value)))
        .collect::<Vec<_>>();
    env_vars.sort();

    DockerSandboxOptions {
        image: settings.image.clone(),
        network_mode: settings.network_mode.clone(),
        memory_limit: settings.memory_limit,
        cpu_quota: settings.cpu_quota,
        env_vars,
        skip_clone: settings.skip_clone,
        ..DockerSandboxOptions::default()
    }
}

fn runtime_hook_definition(definition: &ResolvedHookDefinition) -> fabro_hooks::HookDefinition {
    fabro_hooks::HookDefinition {
        name:       definition.name.clone(),
        event:      match definition.event {
            ResolvedHookEvent::RunStart => fabro_hooks::HookEvent::RunStart,
            ResolvedHookEvent::RunComplete => fabro_hooks::HookEvent::RunComplete,
            ResolvedHookEvent::RunFailed => fabro_hooks::HookEvent::RunFailed,
            ResolvedHookEvent::StageStart => fabro_hooks::HookEvent::StageStart,
            ResolvedHookEvent::StageComplete => fabro_hooks::HookEvent::StageComplete,
            ResolvedHookEvent::StageFailed => fabro_hooks::HookEvent::StageFailed,
            ResolvedHookEvent::StageRetrying => fabro_hooks::HookEvent::StageRetrying,
            ResolvedHookEvent::EdgeSelected => fabro_hooks::HookEvent::EdgeSelected,
            ResolvedHookEvent::ParallelStart => fabro_hooks::HookEvent::ParallelStart,
            ResolvedHookEvent::ParallelComplete => fabro_hooks::HookEvent::ParallelComplete,
            ResolvedHookEvent::SandboxReady => fabro_hooks::HookEvent::SandboxReady,
            ResolvedHookEvent::SandboxCleanup => fabro_hooks::HookEvent::SandboxCleanup,
            ResolvedHookEvent::CheckpointSaved => fabro_hooks::HookEvent::CheckpointSaved,
            ResolvedHookEvent::PreToolUse => fabro_hooks::HookEvent::PreToolUse,
            ResolvedHookEvent::PostToolUse => fabro_hooks::HookEvent::PostToolUse,
            ResolvedHookEvent::PostToolUseFailure => fabro_hooks::HookEvent::PostToolUseFailure,
        },
        command:    definition.command.clone(),
        hook_type:  definition.hook_type.as_ref().map(runtime_hook_type),
        matcher:    definition.matcher.clone(),
        blocking:   definition.blocking,
        timeout_ms: definition.timeout_ms,
        sandbox:    definition.sandbox,
    }
}

fn runtime_hook_type(hook_type: &ResolvedHookType) -> fabro_hooks::HookType {
    match hook_type {
        ResolvedHookType::Command { command } => fabro_hooks::HookType::Command {
            command: command.clone(),
        },
        ResolvedHookType::Http {
            url,
            headers,
            allowed_env_vars,
            tls,
        } => fabro_hooks::HookType::Http {
            url:              url.clone(),
            headers:          headers.clone(),
            allowed_env_vars: allowed_env_vars.clone(),
            tls:              match tls {
                ResolvedTlsMode::Verify => fabro_hooks::TlsMode::Verify,
                ResolvedTlsMode::NoVerify => fabro_hooks::TlsMode::NoVerify,
                ResolvedTlsMode::Off => fabro_hooks::TlsMode::Off,
            },
        },
        ResolvedHookType::Prompt { prompt, model } => fabro_hooks::HookType::Prompt {
            prompt: prompt.clone(),
            model:  model.clone(),
        },
        ResolvedHookType::Agent {
            prompt,
            model,
            max_tool_rounds,
        } => fabro_hooks::HookType::Agent {
            prompt:          prompt.clone(),
            model:           model.clone(),
            max_tool_rounds: *max_tool_rounds,
        },
    }
}

impl RunSession {
    /// Shared engine: initialize, execute, retro, finalize, pull_request.
    async fn run(
        self,
        persisted: Persisted,
        checkpoint: Option<Checkpoint>,
    ) -> Result<Started, Error> {
        let preserve_sandbox = self.preserve_sandbox;
        let on_node = self.on_node.clone();

        let record = persisted.run_spec();
        let run_options = RunOptions {
            settings:         record.settings.clone(),
            run_dir:          persisted.run_dir().to_path_buf(),
            cancel_token:     self.cancel_token,
            run_id:           record.run_id,
            labels:           record.labels.clone(),
            workflow_slug:    record.workflow_slug.clone(),
            github_app:       self.github_app.clone(),
            pre_run_git:      record.git.clone(),
            fork_source_ref:  record.fork_source_ref.clone(),
            base_branch:      record.base_branch().map(str::to_string),
            display_base_sha: None,
            git:              self.git.clone(),
        };

        let last_git_sha: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let sha_clone = Arc::clone(&last_git_sha);
            self.emitter.on_event(move |event| match event {
                event if matches!(&event.body, EventBody::CheckpointCompleted(_)) => {
                    if let EventBody::CheckpointCompleted(props) = &event.body {
                        if let Some(sha) = props.git_commit_sha.as_ref() {
                            *sha_clone.lock().unwrap() = Some(sha.clone());
                        }
                    }
                }
                event if matches!(&event.body, EventBody::RunCompleted(_)) => {
                    if let EventBody::RunCompleted(props) = &event.body {
                        if let Some(sha) = props.final_git_commit_sha.as_ref() {
                            *sha_clone.lock().unwrap() = Some(sha.clone());
                        }
                    }
                }
                event if matches!(&event.body, EventBody::GitCommit(_)) => {
                    if let EventBody::GitCommit(props) = &event.body {
                        *sha_clone.lock().unwrap() = Some(props.sha.clone());
                    }
                }
                _ => {}
            });
        }

        let store_progress_logger = RunEventLogger::new(self.event_sink.clone());
        store_progress_logger.register(self.emitter.as_ref());

        let init_options = InitOptions {
            run_id: record.run_id,
            run_store: self.run_store.clone(),
            dry_run: run_options.dry_run_enabled(),
            emitter: self.emitter,
            sandbox: self.sandbox,
            llm: self.llm,
            interviewer: self.interviewer,
            lifecycle: self.lifecycle,
            run_options,
            workflow_path: self.workflow_path,
            workflow_bundle: self.workflow_bundle,
            hooks: self.hooks,
            sandbox_env: self.sandbox_env,
            vault: self.vault,
            devcontainer: self.devcontainer,
            git: self.git,
            worktree_mode: self.worktree_mode,
            registry_override: self.registry_override,
            artifact_sink: self.artifact_sink,
            run_control: self.run_control,
            checkpoint,
            seed_context: self.seed_context,
        };
        let mut initialized = Box::pin(pipeline::initialize(persisted, init_options)).await?;
        initialized.on_node = on_node;

        let sandbox_for_cleanup = Arc::clone(&initialized.engine.run.sandbox);
        let cleanup_guard = scopeguard::guard((), move |()| {
            if preserve_sandbox {
                return;
            }
            if let Ok(handle) = Handle::try_current() {
                handle.spawn(async move {
                    let _ = sandbox_for_cleanup.cleanup().await;
                });
            }
        });

        let executed = pipeline::execute(initialized).await;
        store_progress_logger.flush().await;
        let final_context = Some(executed.final_context.clone());
        let failed = !matches!(
            executed.outcome.as_ref().map(|outcome| &outcome.status),
            Ok(StageStatus::Success | StageStatus::PartialSuccess)
        );

        let retro_opts = RetroOptions {
            run_id: executed.run_options.run_id,
            services: Arc::clone(&executed.engine.run),
            workflow_name: executed.graph.name.clone(),
            goal: executed.graph.goal().to_string(),
            run_dir: executed.run_options.run_dir.clone(),
            failed,
            run_duration_ms: executed.duration_ms,
            enabled: self.retro_enabled,
            model: executed.model.clone(),
        };

        let retro_start = Instant::now();
        let retroed = Box::pin(pipeline::retro(executed, &retro_opts)).await;
        let retro_duration = retro_start.elapsed();

        let finalize_opts = FinalizeOptions {
            run_dir:          retroed.run_options.run_dir.clone(),
            run_id:           retroed.run_options.run_id,
            workflow_name:    retroed.graph.name.clone(),
            preserve_sandbox: self.preserve_sandbox,
            last_git_sha:     last_git_sha.lock().unwrap().clone(),
        };
        let pr_opts = PullRequestOptions {
            pr_config:  self.pr_config,
            github_app: self.pr_github_app,
            origin_url: self.pr_origin_url,
            model:      self.pr_model,
        };

        let retro = retroed.retro.clone();
        let concluded = Box::pin(pipeline::finalize(retroed, &finalize_opts)).await?;
        let finalized = Box::pin(pipeline::pull_request(concluded, &pr_opts)).await;
        store_progress_logger.flush().await;

        scopeguard::ScopeGuard::into_inner(cleanup_guard);

        Ok(Started {
            finalized,
            final_context,
            retro,
            retro_duration,
        })
    }
}

struct DetachedRunBootstrapGuard {
    run_id:       RunId,
    event_sink:   RunEventSink,
    cancel_token: Option<Arc<AtomicBool>>,
    active:       bool,
}

impl DetachedRunBootstrapGuard {
    fn arm(
        run_id: RunId,
        _run_dir: &Path,
        event_sink: RunEventSink,
        cancel_token: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self {
            run_id,
            event_sink,
            cancel_token,
            active: true,
        }
    }

    fn defuse(&mut self) {
        self.active = false;
    }
}

impl Drop for DetachedRunBootstrapGuard {
    fn drop(&mut self) {
        if self.active {
            let cancelled = self
                .cancel_token
                .as_ref()
                .is_some_and(|token| token.load(Ordering::SeqCst));
            let reason = if cancelled {
                FailureReason::Cancelled
            } else {
                FailureReason::SandboxInitFailed
            };
            let run_id = self.run_id;
            let event_sink = self.event_sink.clone();
            if let Ok(handle) = Handle::try_current() {
                handle.spawn(async move {
                    let _ = append_event_to_sink(&event_sink, &run_id, &Event::WorkflowRunFailed {
                        error: Error::engine(reason.to_string()),
                        duration_ms: 0,
                        reason,
                        git_commit_sha: None,
                        final_patch: None,
                    })
                    .await;
                });
            }
        }
    }
}

const POSTRUN_INTERRUPTED_MESSAGE: &str = "Run interrupted before post-run finalization completed.";
const POSTRUN_CANCELLED_MESSAGE: &str = "Run cancelled before post-run finalization completed.";

struct DetachedRunCompletionGuard {
    event_sink:   RunEventSink,
    run_id:       RunId,
    cancel_token: Option<Arc<AtomicBool>>,
    active:       bool,
}

impl DetachedRunCompletionGuard {
    fn arm(run_id: RunId, event_sink: RunEventSink, cancel_token: Option<Arc<AtomicBool>>) -> Self {
        Self {
            event_sink,
            run_id,
            cancel_token,
            active: true,
        }
    }

    fn defuse(&mut self) {
        self.active = false;
    }
}

impl Drop for DetachedRunCompletionGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        let cancelled = self
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.load(Ordering::SeqCst));
        let reason = if cancelled {
            FailureReason::Cancelled
        } else {
            FailureReason::WorkflowError
        };
        let message = if cancelled {
            POSTRUN_CANCELLED_MESSAGE
        } else {
            POSTRUN_INTERRUPTED_MESSAGE
        };
        let code = if cancelled {
            "postrun_cancelled"
        } else {
            "postrun_interrupted"
        };
        let event_sink = self.event_sink.clone();
        let run_id = self.run_id;
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                let _ = append_event_to_sink(&event_sink, &run_id, &Event::WorkflowRunFailed {
                    error: Error::engine(message.to_string()),
                    duration_ms: 0,
                    reason,
                    git_commit_sha: None,
                    final_patch: None,
                })
                .await;
                let _ = append_event_to_sink(&event_sink, &run_id, &Event::RunNotice {
                    level:   RunNoticeLevel::Error,
                    code:    code.to_string(),
                    message: message.to_string(),
                })
                .await;
            });
        }
    }
}

async fn persist_detached_failure(
    run_id: RunId,
    event_sink: &RunEventSink,
    _run_dir: &Path,
    phase: &'static str,
    reason: FailureReason,
    error: &Error,
) -> Result<(), Error> {
    let message = error.to_string();

    if let Err(err) = append_event_to_sink(event_sink, &run_id, &Event::WorkflowRunFailed {
        error: error.clone(),
        duration_ms: 0,
        reason,
        git_commit_sha: None,
        final_patch: None,
    })
    .await
    {
        tracing::warn!(error = %err, "Failed to append detached failure event");
    }

    let event = Event::RunNotice {
        level:   RunNoticeLevel::Error,
        code:    format!("{phase}_failed"),
        message: message.clone(),
    };
    if let Err(err) = append_event_to_sink(event_sink, &run_id, &event).await {
        tracing::warn!(error = %err, "Failed to append detached failure notice");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chrono::Utc;
    use fabro_config::{RunExecutionLayer, RunLayer, WorkflowSettingsBuilder};
    use fabro_store::Database;
    use fabro_types::settings::run::RunMode;
    use fabro_types::{WorkflowSettings, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::context::Context;
    use crate::event::{Emitter, EventBody};
    use crate::handler::HandlerRegistry;
    use crate::handler::exit::ExitHandler;
    use crate::handler::manager_loop::SubWorkflowHandler;
    use crate::handler::start::StartHandler;
    use crate::operations::resume;
    use crate::records::CheckpointExt;
    use crate::workflow_bundle::{BundledWorkflow, WorkflowBundle};

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    fn storage_root_and_run_dir(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let storage_root = temp.path().join("storage");
        let run_dir = fabro_config::Storage::new(&storage_root)
            .run_scratch(&fixtures::RUN_1)
            .root()
            .to_path_buf();
        (storage_root, run_dir)
    }

    fn settings_from_run_layer(run: RunLayer) -> WorkflowSettings {
        WorkflowSettingsBuilder::new()
            .run_overrides(run)
            .build()
            .expect("settings should resolve")
    }

    async fn persisted_workflow(dot: &str, storage_root: &Path) -> (Persisted, Arc<Database>) {
        let store = memory_store();
        let created = crate::operations::create(
            &store,
            crate::operations::CreateRunInput {
                workflow: crate::operations::WorkflowInput::DotSource {
                    source:   dot.to_string(),
                    base_dir: None,
                },
                settings: settings_from_run_layer(RunLayer {
                    execution: Some(RunExecutionLayer {
                        mode: Some(RunMode::DryRun),
                        ..RunExecutionLayer::default()
                    }),
                    ..RunLayer::default()
                }),
                cwd: storage_root
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf(),
                workflow_slug: Some("test".to_string()),
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_1),
                git: None,
                fork_source_ref: None,
                in_place: false,
                provenance: None,
                configured_providers: Vec::new(),
            },
            storage_root.to_path_buf(),
        )
        .await
        .unwrap();
        (created.persisted, store)
    }

    fn test_registry() -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register("stack.manager_loop", Box::new(SubWorkflowHandler));
        registry
    }

    async fn test_start_services(
        store: &Database,
        _run_dir: &Path,
        emitter: Arc<Emitter>,
        registry: Arc<HandlerRegistry>,
    ) -> StartServices {
        StartServices {
            run_id: fixtures::RUN_1,
            cancel_token: None,
            emitter,
            interviewer: Arc::new(fabro_interview::AutoApproveInterviewer),
            run_store: store.open_run(&fixtures::RUN_1).await.unwrap().into(),
            event_sink: RunEventSink::store(store.open_run(&fixtures::RUN_1).await.unwrap()),
            artifact_sink: None,
            run_control: None,
            github_app: None,
            github_permissions: HashMap::new(),
            vault: None,
            on_node: None,
            registry_override: Some(registry),
        }
    }

    #[tokio::test]
    async fn start_captures_checkpoint_git_sha_in_conclusion() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());
        let injected = Arc::new(AtomicBool::new(false));

        {
            let injected = Arc::clone(&injected);
            let emitter_for_injection = Arc::clone(&emitter);
            emitter.on_event(move |event| {
                if injected.load(Ordering::SeqCst) {
                    return;
                }
                if matches!(&event.body, EventBody::StageStarted(_))
                    && event.node_id.as_deref() == Some("start")
                {
                    injected.store(true, Ordering::SeqCst);
                    emitter_for_injection.emit(&Event::CheckpointCompleted {
                        node_id: "start".to_string(),
                        status: "success".to_string(),
                        current_node: "start".to_string(),
                        completed_nodes: Vec::new(),
                        node_retries: HashMap::new().into_iter().collect(),
                        context_values: HashMap::new().into_iter().collect(),
                        node_outcomes: HashMap::new().into_iter().collect(),
                        next_node_id: None,
                        git_commit_sha: Some("sha-test".to_string()),
                        loop_failure_signatures: HashMap::new().into_iter().collect(),
                        restart_failure_signatures: HashMap::new().into_iter().collect(),
                        node_visits: HashMap::new().into_iter().collect(),
                        diff: None,
                    });
                }
            });
        }

        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;
        let started = start(
            &run_dir,
            test_start_services(&store, &run_dir, emitter, registry).await,
        )
        .await
        .unwrap();

        assert_eq!(
            started.finalized.conclusion.final_git_commit_sha.as_deref(),
            Some("sha-test")
        );
        assert_eq!(started.finalized.conclusion.status, StageStatus::Success);
        assert!(started.retro.is_none());
    }

    #[tokio::test]
    async fn start_loads_persisted_from_run_dir() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;

        let started = start(
            &run_dir,
            test_start_services(&store, &run_dir, emitter, registry).await,
        )
        .await
        .unwrap();

        assert_eq!(started.finalized.conclusion.status, StageStatus::Success);
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        assert!(run_store.state().await.unwrap().conclusion.is_some());
    }

    #[tokio::test]
    async fn start_can_run_bundle_backed_child_workflow_without_workflow_bundle_json() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());
        let store = memory_store();
        let workflow_bundle = WorkflowBundle::new(HashMap::from([
            (PathBuf::from("workflow.fabro"), BundledWorkflow {
                logical_path: PathBuf::from("workflow.fabro"),
                source:       r#"digraph Root {
                        graph [goal="Bundle child"]
                        start [shape=Mdiamond]
                        manager [
                            type="stack.manager_loop",
                            stack.child_workflow="./children/review.fabro",
                            manager.max_cycles=100,
                            manager.poll_interval="10ms"
                        ]
                        exit [shape=Msquare]
                        start -> manager -> exit
                    }"#
                .to_string(),
                files:        HashMap::new(),
            }),
            (PathBuf::from("children/review.fabro"), BundledWorkflow {
                logical_path: PathBuf::from("children/review.fabro"),
                source:       r"digraph Review {
                        start [shape=Mdiamond]
                        exit [shape=Msquare]
                        start -> exit
                    }"
                .to_string(),
                files:        HashMap::new(),
            }),
        ]));

        crate::operations::create(
            &store,
            crate::operations::CreateRunInput {
                workflow: crate::operations::WorkflowInput::Bundled(
                    workflow_bundle
                        .workflow(Path::new("workflow.fabro"))
                        .unwrap()
                        .clone(),
                ),
                settings: settings_from_run_layer(RunLayer {
                    execution: Some(RunExecutionLayer {
                        mode: Some(RunMode::DryRun),
                        ..RunExecutionLayer::default()
                    }),
                    ..RunLayer::default()
                }),
                cwd: temp.path().to_path_buf(),
                workflow_slug: Some("bundle-child".to_string()),
                workflow_path: Some(PathBuf::from("workflow.fabro")),
                workflow_bundle: Some(workflow_bundle),
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_1),
                git: None,
                fork_source_ref: None,
                in_place: false,
                provenance: None,
                configured_providers: Vec::new(),
            },
            storage_root,
        )
        .await
        .unwrap();

        let started = start(
            &run_dir,
            test_start_services(&store, &run_dir, emitter, registry).await,
        )
        .await
        .unwrap();

        assert_eq!(started.finalized.conclusion.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn start_invokes_on_node_callback_before_execution() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());
        let visited = Arc::new(Mutex::new(Vec::new()));

        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;

        let started = start(&run_dir, StartServices {
            on_node: Some(Arc::new({
                let visited = Arc::clone(&visited);
                move |node_id: &str| {
                    visited.lock().unwrap().push(node_id.to_string());
                }
            })),
            ..test_start_services(&store, &run_dir, emitter, registry).await
        })
        .await
        .unwrap();

        assert_eq!(started.finalized.conclusion.status, StageStatus::Success);
        assert_eq!(*visited.lock().unwrap(), vec!["start".to_string()]);
    }

    #[tokio::test]
    async fn start_errors_when_checkpoint_exists() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;
        let services = test_start_services(&store, &run_dir, emitter, registry).await;

        // Seed an authoritative checkpoint event so start() sees it
        let checkpoint = Checkpoint {
            timestamp:                  chrono::Utc::now(),
            current_node:               "start".into(),
            completed_nodes:            vec!["start".to_string()],
            node_retries:               HashMap::new(),
            context_values:             Context::new().snapshot(),
            node_outcomes:              HashMap::new(),
            next_node_id:               Some("exit".to_string()),
            git_commit_sha:             None,
            loop_failure_signatures:    HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits:                HashMap::new(),
        };
        crate::event::append_event(
            &store.open_run(&fixtures::RUN_1).await.unwrap(),
            &services.run_id,
            &Event::CheckpointCompleted {
                node_id: checkpoint.current_node.clone(),
                status: checkpoint
                    .node_outcomes
                    .get(&checkpoint.current_node)
                    .map_or_else(
                        || "success".to_string(),
                        |outcome| outcome.status.to_string(),
                    ),
                current_node: checkpoint.current_node.clone(),
                completed_nodes: checkpoint.completed_nodes.clone(),
                node_retries: checkpoint.node_retries.clone().into_iter().collect(),
                context_values: checkpoint.context_values.clone().into_iter().collect(),
                node_outcomes: checkpoint.node_outcomes.clone().into_iter().collect(),
                next_node_id: checkpoint.next_node_id.clone(),
                git_commit_sha: checkpoint.git_commit_sha.clone(),
                loop_failure_signatures: checkpoint
                    .loop_failure_signatures
                    .iter()
                    .map(|(sig, count)| (sig.to_string(), *count))
                    .collect(),
                restart_failure_signatures: checkpoint
                    .restart_failure_signatures
                    .iter()
                    .map(|(sig, count)| (sig.to_string(), *count))
                    .collect(),
                node_visits: checkpoint.node_visits.clone().into_iter().collect(),
                diff: None,
            },
        )
        .await
        .unwrap();

        let result = start(&run_dir, services).await;

        assert!(
            matches!(&result, Err(crate::error::Error::Precondition(_))),
            "expected Precondition error, got: {result:?}",
            result = result.as_ref().map(|_| "Ok"),
        );
    }

    #[tokio::test]
    async fn resume_errors_when_checkpoint_missing() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;

        let result = resume(
            &run_dir,
            test_start_services(&store, &run_dir, emitter, registry).await,
        )
        .await;

        assert!(
            matches!(&result, Err(crate::error::Error::Precondition(_))),
            "expected Precondition error, got: {result:?}",
            result = result.as_ref().map(|_| "Ok"),
        );
    }

    #[tokio::test]
    async fn resume_errors_when_run_already_finished_successfully() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        std::fs::create_dir_all(&run_dir).unwrap();
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;

        let checkpoint = Checkpoint::from_context(
            &Context::new(),
            "start",
            vec!["start".to_string()],
            HashMap::new(),
            HashMap::new(),
            Some("exit".to_string()),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let conclusion = crate::records::Conclusion {
            timestamp:            Utc::now(),
            status:               StageStatus::Success,
            duration_ms:          1,
            failure_reason:       None,
            final_git_commit_sha: None,
            stages:               vec![],
            billing:              None,
            total_retries:        0,
        };
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        crate::event::append_event(&run_store, &fixtures::RUN_1, &Event::CheckpointCompleted {
            node_id: checkpoint.current_node.clone(),
            status: "success".to_string(),
            current_node: checkpoint.current_node.clone(),
            completed_nodes: checkpoint.completed_nodes.clone(),
            node_retries: checkpoint.node_retries.clone().into_iter().collect(),
            context_values: checkpoint.context_values.clone().into_iter().collect(),
            node_outcomes: checkpoint.node_outcomes.clone().into_iter().collect(),
            next_node_id: checkpoint.next_node_id.clone(),
            git_commit_sha: checkpoint.git_commit_sha.clone(),
            loop_failure_signatures: checkpoint
                .loop_failure_signatures
                .iter()
                .map(|(sig, count)| (sig.to_string(), *count))
                .collect(),
            restart_failure_signatures: checkpoint
                .restart_failure_signatures
                .iter()
                .map(|(sig, count)| (sig.to_string(), *count))
                .collect(),
            node_visits: checkpoint.node_visits.clone().into_iter().collect(),
            diff: None,
        })
        .await
        .unwrap();
        crate::event::append_event(&run_store, &fixtures::RUN_1, &Event::RunStarting)
            .await
            .unwrap();
        crate::event::append_event(&run_store, &fixtures::RUN_1, &Event::RunRunning)
            .await
            .unwrap();
        crate::event::append_event(&run_store, &fixtures::RUN_1, &Event::WorkflowRunCompleted {
            duration_ms:          conclusion.duration_ms,
            artifact_count:       0,
            status:               "success".to_string(),
            reason:               crate::run_status::SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            billing:              None,
        })
        .await
        .unwrap();

        let result = resume(
            &run_dir,
            test_start_services(&store, &run_dir, emitter, registry).await,
        )
        .await;

        assert!(
            matches!(&result, Err(crate::error::Error::Precondition(_))),
            "expected Precondition error, got: {result:?}",
            result = result.as_ref().map(|_| "Ok"),
        );
    }
}
