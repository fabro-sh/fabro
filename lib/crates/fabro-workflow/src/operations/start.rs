use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fabro_auth::{CredentialSource, EnvCredentialSource, VaultCredentialSource};
use fabro_interview::{AutoApproveInterviewer, Interviewer};
use fabro_llm::client::Client as LlmClient;
use fabro_mcp::config::{McpServerSettings, McpTransport};
use fabro_model::{Catalog, FallbackTarget, ProviderId};
use fabro_sandbox::daytona::DaytonaConfig;
use fabro_sandbox::from_environment::{
    daytona_config_from_environment, docker_config_from_environment,
};
use fabro_sandbox::{DockerSandboxOptions, SandboxSpec};
use fabro_static::EnvVars;
use fabro_types::settings::run::{
    ApprovalMode, HookDefinition as ResolvedHookDefinition, HookEvent as ResolvedHookEvent,
    HookType as ResolvedHookType, McpServerSettings as ResolvedMcpServerSettings,
    McpTransport as ResolvedMcpTransport, PullRequestSettings, RunMode,
    RunModelSettings as ResolvedRunModelSettings, RunNamespace as ResolvedRunSettings,
    TlsMode as ResolvedTlsMode,
};
use fabro_types::settings::{InterpString, ModelRegistry, ResolvedModelRef};
use fabro_types::{ManifestPath, RunId, RunRunnableSource, SandboxProviderKind};
use fabro_vault::Vault;
use tokio::runtime::Handle;
use tokio::sync::RwLock as AsyncRwLock;
use tokio_util::sync::CancellationToken;

use crate::artifact_upload::ArtifactSink;
use crate::context::Context;
use crate::error::{self, Error};
use crate::event::{
    Emitter, Event, EventBody, RunEventLogger, RunEventSink, RunNoticeLevel, append_event_to_sink,
};
use crate::handler::HandlerRegistry;
use crate::handler::llm::routing;
use crate::outcome::{Outcome, StageOutcome};
use crate::pipeline::{
    self, FinalizeOptions, Finalized, InitOptions, LlmSpec, Persisted, PullRequestOptions,
    SandboxEnvSpec, build_conclusion_from_store, classify_engine_result,
};
use crate::records::Checkpoint;
use crate::run_control::RunControlState;
use crate::run_metadata::metadata_branch_name;
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions};
use crate::run_status::{FailureReason, RunStatus};
use crate::runtime_store::RunStoreHandle;
use crate::services::FabroRunToolServices;
use crate::steering_hub::SteeringHub;
use crate::workflow_bundle::{RunDefinition, WorkflowBundle};

struct RunSession {
    cancel_token:      CancellationToken,
    emitter:           Arc<Emitter>,
    sandbox:           SandboxSpec,
    llm:               LlmSpec,
    interviewer:       Arc<dyn Interviewer>,
    steering_hub:      Arc<SteeringHub>,
    on_node:           crate::OnNodeCallback,
    lifecycle:         LifecycleOptions,
    hooks:             fabro_hooks::HookSettings,
    sandbox_env:       SandboxEnvSpec,
    seed_context:      Option<Context>,
    run_store:         RunStoreHandle,
    event_sink:        RunEventSink,
    artifact_sink:     Option<ArtifactSink>,
    git:               Option<GitCheckpointOptions>,
    github_app:        Option<fabro_github::GitHubCredentials>,
    registry_override: Option<Arc<HandlerRegistry>>,
    preserve_sandbox:  bool,
    stop_on_terminal:  bool,
    pr_config:         Option<PullRequestSettings>,
    pr_github_app:     Option<fabro_github::GitHubCredentials>,
    pr_origin_url:     Option<String>,
    pr_model:          String,
    workflow_path:     Option<ManifestPath>,
    workflow_bundle:   Option<Arc<WorkflowBundle>>,
    run_control:       Option<Arc<RunControlState>>,
    vault:             Option<Arc<AsyncRwLock<Vault>>>,
    catalog:           Arc<Catalog>,
    fabro_run_tools:   Option<FabroRunToolServices>,
}

struct ResolvedStartLlm {
    model:          String,
    provider_id:    ProviderId,
    fallback_chain: Vec<FallbackTarget>,
}

pub struct StartServices {
    pub run_id:             RunId,
    pub cancel_token:       CancellationToken,
    pub emitter:            Arc<Emitter>,
    pub interviewer:        Arc<dyn Interviewer>,
    pub steering_hub:       Arc<SteeringHub>,
    pub run_store:          RunStoreHandle,
    pub event_sink:         RunEventSink,
    pub artifact_sink:      Option<ArtifactSink>,
    pub run_control:        Option<Arc<RunControlState>>,
    pub github_app:         Option<fabro_github::GitHubCredentials>,
    /// Server-resolved GitHub integration permissions to inject into the
    /// sandbox env. Empty when github integration has no permissions.
    pub github_permissions: HashMap<String, String>,
    pub vault:              Option<Arc<AsyncRwLock<Vault>>>,
    pub catalog:            Arc<Catalog>,
    pub on_node:            crate::OnNodeCallback,
    pub registry_override:  Option<Arc<HandlerRegistry>>,
    pub fabro_run_tools:    Option<FabroRunToolServices>,
}

pub struct Started {
    pub finalized:     Finalized,
    pub final_context: Option<Context>,
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
    if state.current_checkpoint().is_some() {
        return Err(Error::Precondition(
            "checkpoint already exists in the run store — did you mean to resume?".to_string(),
        ));
    }

    let status = state.status;
    if !matches!(
        status,
        RunStatus::Submitted | RunStatus::Runnable | RunStatus::Starting
    ) {
        return Err(Error::Precondition(format!(
            "cannot start run: status is {status}, expected submitted or runnable"
        )));
    }
    if matches!(status, RunStatus::Submitted) {
        append_event_to_sink(
            &services.event_sink,
            &services.run_id,
            &Event::RunStartRequested {
                resume: false,
                actor:  None,
            },
        )
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
        append_event_to_sink(
            &services.event_sink,
            &services.run_id,
            &Event::RunRunnable {
                source: RunRunnableSource::StartRequested,
                actor:  None,
            },
        )
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
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
            &run_store,
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
            &run_store,
            &event_sink,
            run_dir,
            "bootstrap",
            FailureReason::BootstrapFailed,
            &error,
        )
        .await;
        return Err(error);
    }

    let mut bootstrap_guard = DetachedRunBootstrapGuard::arm(
        run_id,
        run_store.clone(),
        event_sink.clone(),
        cancel_token.clone(),
    );

    let persisted = match Persisted::load_from_store(&services.run_store, run_dir).await {
        Ok(persisted) => persisted,
        Err(err) => {
            let _ = persist_detached_failure(
                run_id,
                &run_store,
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
                &run_store,
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
    let mut completion_guard = DetachedRunCompletionGuard::arm(
        run_id,
        run_store.clone(),
        event_sink.clone(),
        cancel_token,
    );
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

/// Build a conclusion from the store and emit `run.failed` carrying the
/// rolled-up timing and billing. Shared by the engine-failure terminal path,
/// the bootstrap/completion drop guards, and `persist_detached_failure`.
async fn emit_workflow_run_failed(
    run_id: RunId,
    run_store: &RunStoreHandle,
    event_sink: &RunEventSink,
    error: &Error,
    reason: FailureReason,
    wall_duration_ms: u64,
) {
    let failure = Some(error::run_failure_from_error(error, reason));
    let conclusion = build_conclusion_from_store(
        run_store,
        StageOutcome::Failed {
            retry_requested: false,
        },
        failure,
        wall_duration_ms,
        None,
    )
    .await;
    let failure_event = Event::workflow_run_failed_from_error(
        error,
        conclusion.timing,
        reason,
        None,
        None,
        None,
        conclusion.billing,
    );
    if let Err(err) = append_event_to_sink(event_sink, &run_id, &failure_event).await {
        tracing::warn!(error = %err, "Failed to append run.failed event");
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
    let (_, _, run_status) = classify_engine_result(&engine_result);
    let reason = match run_status {
        RunStatus::Failed { reason } => reason,
        _ => FailureReason::WorkflowError,
    };
    emit_workflow_run_failed(
        run_id,
        run_store,
        event_sink,
        error,
        reason,
        crate::millis_u64(duration),
    )
    .await;
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
        let git = git_checkpoint_options_from_start(settings, &record.run_id, state.start);
        let definition_blob = state.spec.definition_blob;
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

        let sandbox_provider =
            resolve_sandbox_provider(resolved).effective_for(resolved.execution.mode);
        let catalog = Arc::clone(&services.catalog);
        let configured =
            configured_providers_for_start(services.vault.as_ref(), Arc::clone(&catalog)).await;
        let llm = resolve_start_llm(catalog.as_ref(), &configured, resolved)?;
        let mcp_servers = resolved
            .agent
            .mcps
            .values()
            .map(runtime_mcp_server)
            .collect();

        let sandbox = match sandbox_provider {
            SandboxProviderKind::Local => SandboxSpec::Local {
                working_directory: working_directory.clone(),
            },
            SandboxProviderKind::Docker => SandboxSpec::Docker {
                config:           resolve_docker_config(resolved),
                github_app:       services.github_app.clone(),
                run_id:           Some(record.run_id),
                clone_origin_url: record.repo_origin_url().map(str::to_string),
                clone_branch:     record.base_branch().map(str::to_string),
            },
            SandboxProviderKind::Daytona => {
                let api_key = match &services.vault {
                    Some(v) => v
                        .read()
                        .await
                        .get(EnvVars::DAYTONA_API_KEY)
                        .map(str::to_string),
                    None => None,
                };
                SandboxSpec::Daytona {
                    config: Box::new(resolve_daytona_config(resolved)),
                    github_app: services.github_app.clone(),
                    run_id: Some(record.run_id),
                    clone_origin_url: record.repo_origin_url().map(str::to_string),
                    clone_branch: record.base_branch().map(str::to_string),
                    api_key,
                }
            }
        };

        let toml_env = resolved.environment.resolve_env(process_env_var);
        let github_permissions: Option<HashMap<String, String>> =
            (!services.github_permissions.is_empty()).then(|| services.github_permissions.clone());
        let sandbox_env = SandboxEnvSpec {
            toml_env,
            github_permissions,
            origin_url: record.repo_origin_url().map(str::to_string),
        };

        let interviewer: Arc<dyn Interviewer> = if resolved.execution.approval == ApprovalMode::Auto
        {
            Arc::new(AutoApproveInterviewer::engine())
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
                model: llm.model.clone(),
                provider_id: llm.provider_id.clone(),
                fallback_chain: llm.fallback_chain,
                mcp_servers,
                model_controls: resolved.model.controls.clone(),
                dry_run: resolved.execution.mode == RunMode::DryRun,
            },
            interviewer,
            steering_hub: services.steering_hub,
            on_node: services.on_node,
            lifecycle: LifecycleOptions {
                setup_commands:           resolved.prepare.commands.clone(),
                setup_command_timeout_ms: resolved.prepare.timeout_ms,
            },
            hooks: fabro_hooks::HookSettings {
                hooks: resolved.hooks.iter().map(runtime_hook_definition).collect(),
            },
            sandbox_env,
            seed_context: None,
            run_store: services.run_store,
            artifact_sink: services.artifact_sink,
            git,
            github_app: services.github_app.clone(),
            registry_override: services.registry_override,
            preserve_sandbox: resolved.environment.lifecycle.preserve,
            stop_on_terminal: resolved.environment.lifecycle.stop_on_terminal,
            pr_config,
            pr_github_app: services.github_app,
            pr_origin_url: record.repo_origin_url().map(str::to_string),
            pr_model: llm.model,
            workflow_path,
            workflow_bundle,
            vault: services.vault,
            catalog,
            fabro_run_tools: services.fabro_run_tools,
        })
    }
}

async fn configured_providers_for_start(
    vault: Option<&Arc<AsyncRwLock<Vault>>>,
    catalog: Arc<Catalog>,
) -> Vec<ProviderId> {
    let source: Arc<dyn CredentialSource> = match vault {
        Some(vault) => Arc::new(VaultCredentialSource::with_env_lookup(
            Arc::clone(vault),
            process_env_var,
        )),
        None => Arc::new(EnvCredentialSource::new()),
    };
    match LlmClient::from_source_report(source.as_ref(), catalog).await {
        Ok(report) => report
            .client
            .provider_names()
            .into_iter()
            .map(ProviderId::new)
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn git_checkpoint_options_from_start(
    settings: &fabro_types::WorkflowSettings,
    run_id: &RunId,
    start: Option<fabro_types::StartRecord>,
) -> Option<GitCheckpointOptions> {
    if !settings.run.run_branch.enabled {
        return None;
    }

    let start = start?;
    start.run_branch.as_ref().map(|_| GitCheckpointOptions {
        base_sha:    start.base_sha.clone(),
        run_branch:  start.run_branch.clone(),
        meta_branch: settings
            .run
            .meta_branch
            .enabled
            .then(|| metadata_branch_name(&run_id.to_string())),
    })
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

fn resolve_sandbox_provider(settings: &ResolvedRunSettings) -> SandboxProviderKind {
    SandboxProviderKind::from(settings.environment.provider)
}

fn resolve_daytona_config(settings: &ResolvedRunSettings) -> DaytonaConfig {
    daytona_config_from_environment(&settings.environment, !settings.clone.enabled)
}

fn resolve_docker_config(settings: &ResolvedRunSettings) -> DockerSandboxOptions {
    docker_config_from_environment(&settings.environment, !settings.clone.enabled)
}

fn resolve_start_llm(
    catalog: &Catalog,
    configured: &[ProviderId],
    settings: &ResolvedRunSettings,
) -> Result<ResolvedStartLlm, Error> {
    let model = settings.model.name.as_ref().map_or_else(
        || catalog.default_for_configured_ids(configured).id.clone(),
        InterpString::as_source,
    );
    let provider = settings
        .model
        .provider
        .as_ref()
        .map(InterpString::as_source)
        .filter(|value| !value.is_empty());

    let default_provider_id = catalog
        .default_for_configured_ids(configured)
        .provider
        .clone();
    let provider_context = routing::resolve_provider_context(
        catalog,
        &default_provider_id,
        &model,
        provider.as_deref(),
    )?;
    let provider_id = provider_context.provider_id;
    let fallback_chain = resolve_fallback_chain(catalog, &provider_id, &model, &settings.model);

    Ok(ResolvedStartLlm {
        model,
        provider_id,
        fallback_chain,
    })
}

fn resolve_fallback_chain(
    catalog: &Catalog,
    _provider: &ProviderId,
    model: &str,
    settings: &ResolvedRunModelSettings,
) -> Vec<FallbackTarget> {
    if settings.fallbacks.is_empty() {
        return Vec::new();
    }
    let registry = CatalogModelRegistry { catalog };
    let primary = catalog.get(model);

    settings
        .fallbacks
        .iter()
        .filter_map(|model_ref| match model_ref.resolve(&registry).ok()? {
            ResolvedModelRef::Provider(provider_name) => {
                let provider_id = canonical_provider_id(catalog, &provider_name);
                let reference = primary?;
                catalog
                    .closest(&provider_id, reference)
                    .map(|model| FallbackTarget {
                        provider: provider_id.to_string(),
                        model:    model.id.clone(),
                    })
            }
            ResolvedModelRef::Model { provider, model } => {
                let provider =
                    provider.map(|provider| canonical_provider_id(catalog, &provider).to_string());
                if let Some(info) = catalog.get(&model) {
                    let provider = provider.unwrap_or_else(|| info.provider.to_string());
                    return Some(FallbackTarget {
                        provider,
                        model: info.id.clone(),
                    });
                }
                provider.map(|provider| FallbackTarget { provider, model })
            }
        })
        .collect()
}

fn canonical_provider_id(catalog: &Catalog, provider_name: &str) -> ProviderId {
    let provider_id = ProviderId::from(provider_name);
    catalog
        .provider(&provider_id)
        .map_or(provider_id, |provider| provider.id.clone())
}

struct CatalogModelRegistry<'a> {
    catalog: &'a Catalog,
}

impl ModelRegistry for CatalogModelRegistry<'_> {
    fn is_provider(&self, token: &str) -> bool {
        self.catalog.provider(&ProviderId::from(token)).is_some()
    }

    fn is_model(&self, token: &str) -> bool {
        self.catalog.get(token).is_some()
    }

    fn provider_of(&self, token: &str) -> Option<String> {
        self.catalog
            .get(token)
            .map(|model| model.provider.to_string())
    }
}

fn runtime_mcp_server(settings: &ResolvedMcpServerSettings) -> McpServerSettings {
    McpServerSettings {
        name:                 settings.name.clone(),
        transport:            match &settings.transport {
            ResolvedMcpTransport::Stdio { command, env } => McpTransport::Stdio {
                command: command.clone(),
                env:     env.clone(),
            },
            ResolvedMcpTransport::Http {
                protocol,
                url,
                headers,
            } => McpTransport::Http {
                protocol: *protocol,
                url:      url.clone(),
                headers:  headers.clone(),
            },
            ResolvedMcpTransport::Sandbox {
                protocol,
                command,
                port,
                env,
            } => McpTransport::Sandbox {
                protocol: *protocol,
                command:  command.clone(),
                port:     *port,
                env:      env.clone(),
            },
        },
        current_dir:          None,
        clear_env:            false,
        startup_timeout_secs: settings.startup_timeout_secs,
        tool_timeout_secs:    settings.tool_timeout_secs,
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
    /// Shared engine: initialize, execute, finalize, pull_request.
    async fn run(
        self,
        persisted: Persisted,
        checkpoint: Option<Checkpoint>,
    ) -> Result<Started, Error> {
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
                            *sha_clone.lock()
                                .expect("sha_clone mutex should not be poisoned: no code panics while holding this lock") = Some(sha.clone());
                        }
                    }
                }
                event if matches!(&event.body, EventBody::RunCompleted(_)) => {
                    if let EventBody::RunCompleted(props) = &event.body {
                        if let Some(sha) = props.final_git_commit_sha.as_ref() {
                            *sha_clone.lock()
                                .expect("sha_clone mutex should not be poisoned: no code panics while holding this lock") = Some(sha.clone());
                        }
                    }
                }
                event if matches!(&event.body, EventBody::RunFailed(_)) => {
                    if let EventBody::RunFailed(props) = &event.body {
                        if let Some(sha) = props.final_git_commit_sha.as_ref() {
                            *sha_clone.lock()
                                .expect("sha_clone mutex should not be poisoned: no code panics while holding this lock") = Some(sha.clone());
                        }
                    }
                }
                event if matches!(&event.body, EventBody::GitCommit(_)) => {
                    if let EventBody::GitCommit(props) = &event.body {
                        *sha_clone.lock()
                            .expect("sha_clone mutex should not be poisoned: no code panics while holding this lock") = Some(props.sha.clone());
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
            steering_hub: Arc::clone(&self.steering_hub),
            catalog: Arc::clone(&self.catalog),
            lifecycle: self.lifecycle,
            run_options,
            workflow_path: self.workflow_path,
            workflow_bundle: self.workflow_bundle,
            hooks: self.hooks,
            sandbox_env: self.sandbox_env,
            vault: self.vault,
            git: self.git,
            registry_override: self.registry_override,
            artifact_sink: self.artifact_sink,
            run_control: self.run_control,
            checkpoint,
            seed_context: self.seed_context,
            fabro_run_tools: self.fabro_run_tools,
        };
        let mut initialized = Box::pin(pipeline::initialize(persisted, init_options)).await?;
        initialized.on_node = on_node;

        let sandbox_for_cleanup = Arc::clone(&initialized.engine.run.sandbox);
        let stop_on_terminal = self.stop_on_terminal;
        let cleanup_guard = scopeguard::guard((), move |()| {
            if !stop_on_terminal {
                return;
            }
            if let Ok(handle) = Handle::try_current() {
                handle.spawn(async move {
                    let _ = sandbox_for_cleanup.stop().await;
                });
            }
        });

        // Drain any unconsumed pending steers on every exit path
        // (success, error, panic). The emit lands in the progress log via
        // the explicit flush below; the scopeguard is a panic-only fallback.
        let steering_hub_for_drain = Arc::clone(&self.steering_hub);
        let _drain_guard = scopeguard::guard((), move |()| {
            steering_hub_for_drain.drain_pending_at_run_end();
        });

        let executed = pipeline::execute(initialized).await;
        store_progress_logger.flush().await;
        let final_context = Some(executed.final_context.clone());

        let finalize_opts = FinalizeOptions {
            run_dir:          executed.run_options.run_dir.clone(),
            run_id:           executed.run_options.run_id,
            workflow_name:    executed.graph.name.clone(),
            preserve_sandbox: self.preserve_sandbox,
            stop_on_terminal: self.stop_on_terminal,
            last_git_sha:     last_git_sha.lock()
                .expect("last_git_sha mutex should not be poisoned: no code panics while holding this lock")
                .clone(),
        };
        let pr_opts = PullRequestOptions {
            pr_config:  self.pr_config,
            github_app: self.pr_github_app,
            origin_url: self.pr_origin_url,
            model:      self.pr_model,
        };

        let concluded = match Box::pin(pipeline::finalize(executed, &finalize_opts)).await {
            Ok(concluded) => concluded,
            Err(err) => {
                self.steering_hub.drain_pending_at_run_end();
                store_progress_logger.flush().await;
                return Err(err);
            }
        };
        let finalized = Box::pin(pipeline::pull_request(concluded, &pr_opts)).await;
        // Emit `agent.steer.dropped { reason: run_ended }` for any
        // unconsumed pending steers on the success path, then flush. The
        // scopeguard above re-runs as a no-op (drain is idempotent on an
        // already-empty buffer) on the way out of scope.
        self.steering_hub.drain_pending_at_run_end();
        store_progress_logger.flush().await;

        scopeguard::ScopeGuard::into_inner(cleanup_guard);

        Ok(Started {
            finalized,
            final_context,
        })
    }
}

struct DetachedRunBootstrapGuard {
    run_id:       RunId,
    run_store:    RunStoreHandle,
    event_sink:   RunEventSink,
    cancel_token: CancellationToken,
    active:       bool,
}

impl DetachedRunBootstrapGuard {
    fn arm(
        run_id: RunId,
        run_store: RunStoreHandle,
        event_sink: RunEventSink,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            run_id,
            run_store,
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
        if !self.active {
            return;
        }
        let reason = if self.cancel_token.is_cancelled() {
            FailureReason::Cancelled
        } else {
            FailureReason::SandboxInitFailed
        };
        let run_id = self.run_id;
        let run_store = self.run_store.clone();
        let event_sink = self.event_sink.clone();
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                emit_workflow_run_failed(
                    run_id,
                    &run_store,
                    &event_sink,
                    &Error::engine(reason.to_string()),
                    reason,
                    0,
                )
                .await;
            });
        }
    }
}

const POSTRUN_INTERRUPTED_MESSAGE: &str = "Run interrupted before post-run finalization completed.";
const POSTRUN_CANCELLED_MESSAGE: &str = "Run cancelled before post-run finalization completed.";

struct DetachedRunCompletionGuard {
    event_sink:   RunEventSink,
    run_id:       RunId,
    run_store:    RunStoreHandle,
    cancel_token: CancellationToken,
    active:       bool,
}

impl DetachedRunCompletionGuard {
    fn arm(
        run_id: RunId,
        run_store: RunStoreHandle,
        event_sink: RunEventSink,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            event_sink,
            run_id,
            run_store,
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

        let cancelled = self.cancel_token.is_cancelled();
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
        let run_store = self.run_store.clone();
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                emit_workflow_run_failed(
                    run_id,
                    &run_store,
                    &event_sink,
                    &Error::engine(message.to_string()),
                    reason,
                    0,
                )
                .await;
                let _ = append_event_to_sink(&event_sink, &run_id, &Event::RunNotice {
                    level:            RunNoticeLevel::Error,
                    code:             code.to_string(),
                    message:          message.to_string(),
                    exec_output_tail: None,
                })
                .await;
            });
        }
    }
}

async fn persist_detached_failure(
    run_id: RunId,
    run_store: &RunStoreHandle,
    event_sink: &RunEventSink,
    _run_dir: &Path,
    phase: &'static str,
    reason: FailureReason,
    error: &Error,
) -> Result<(), Error> {
    emit_workflow_run_failed(run_id, run_store, event_sink, error, reason, 0).await;

    let event = Event::RunNotice {
        level:            RunNoticeLevel::Error,
        code:             format!("{phase}_failed"),
        message:          error.to_string(),
        exec_output_tail: None,
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
    use fabro_config::{
        EnvironmentImageLayer, EnvironmentNetworkLayer, EnvironmentResourcesLayer,
        EnvironmentVolumeLayer, RunCloneLayer, RunEnvironmentLayer, RunExecutionLayer, RunLayer,
        StickyMap, WorkflowSettingsBuilder,
    };
    use fabro_store::Database;
    use fabro_types::settings::run::RunMode;
    use fabro_types::settings::{InterpString, ModelRef};
    use fabro_types::{BilledModelUsage, ManifestPath, StageTiming, WorkflowSettings, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::context::Context;
    use crate::event::{Emitter, EventBody};
    use crate::handler::exit::ExitHandler;
    use crate::handler::manager_loop::SubWorkflowHandler;
    use crate::handler::start::StartHandler;
    use crate::handler::{EngineServices, Handler, HandlerRegistry};
    use crate::operations::resume;
    use crate::outcome::{Outcome, StageOutcome};
    use crate::records::CheckpointExt;
    use crate::workflow_bundle::{BundledWorkflow, WorkflowBundle};

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    const TIMED_DOT: &str = r#"digraph Test {
        graph [goal="Time active work"]
        start [shape=Mdiamond]
        work  [type="timed"]
        exit  [shape=Msquare]
        start -> work
        work -> exit
    }"#;

    struct TimedOutcomeHandler;

    fn timed_success_outcome() -> Outcome {
        let mut outcome = Outcome::success();
        outcome.timing = Some(StageTiming::new(0, 100, 50));
        outcome
    }

    #[async_trait::async_trait]
    impl Handler for TimedOutcomeHandler {
        async fn execute(
            &self,
            _node: &fabro_graphviz::graph::Node,
            _context: &Context,
            _graph: &fabro_graphviz::graph::Graph,
            _run_dir: &Path,
            _services: &EngineServices,
        ) -> Result<Outcome, Error> {
            Ok(timed_success_outcome())
        }

        async fn simulate(
            &self,
            _node: &fabro_graphviz::graph::Node,
            _context: &Context,
            _graph: &fabro_graphviz::graph::Graph,
            _run_dir: &Path,
            _services: &EngineServices,
        ) -> Result<Outcome, Error> {
            Ok(timed_success_outcome())
        }
    }

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

    fn test_catalog() -> Arc<Catalog> {
        Arc::new(Catalog::from_builtin().expect("default catalog should build"))
    }

    #[test]
    fn resolve_fallback_chain_resolves_provider_fallbacks() {
        let catalog = test_catalog();
        let settings = ResolvedRunModelSettings {
            fallbacks: vec!["openai".parse::<ModelRef>().unwrap()],
            ..ResolvedRunModelSettings::default()
        };

        let chain = resolve_fallback_chain(
            catalog.as_ref(),
            &ProviderId::anthropic(),
            "claude-opus-4-6",
            &settings,
        );

        assert_eq!(chain, vec![FallbackTarget {
            provider: "openai".to_string(),
            model:    "gpt-5.5".to_string(),
        }]);
    }

    #[test]
    fn resolve_fallback_chain_resolves_explicit_model_fallbacks() {
        let catalog = test_catalog();
        let settings = ResolvedRunModelSettings {
            fallbacks: vec!["openai/gpt-5.4-mini".parse::<ModelRef>().unwrap()],
            ..ResolvedRunModelSettings::default()
        };

        let chain = resolve_fallback_chain(
            catalog.as_ref(),
            &ProviderId::anthropic(),
            "claude-opus-4-6",
            &settings,
        );

        assert_eq!(chain, vec![FallbackTarget {
            provider: "openai".to_string(),
            model:    "gpt-5.4-mini".to_string(),
        }]);
    }

    #[test]
    fn resolve_start_llm_infers_provider_from_model_alias() {
        let overrides: fabro_model::catalog::LlmCatalogSettings = toml::from_str(
            r#"
[providers.acme]
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[models.acme-claude]
provider = "acme"
display_name = "Acme Claude"
family = "claude"
default = true
agent_profile = "anthropic"
aliases = ["ac"]

[models.acme-claude.limits]
context_window = 1000

[models.acme-claude.features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        let catalog = Catalog::from_builtin_with_overrides(&overrides).unwrap();
        let mut settings = ResolvedRunSettings::default();
        settings.model.name = Some(InterpString::parse("ac"));

        let resolved = resolve_start_llm(&catalog, &[], &settings).unwrap();

        assert_eq!(resolved.model, "ac");
        assert_eq!(resolved.provider_id, ProviderId::new("acme"));
    }

    #[test]
    fn runtime_clone_config_uses_run_level_clone_policy() {
        let settings = settings_from_run_layer(RunLayer {
            clone: Some(RunCloneLayer {
                enabled: Some(false),
            }),
            ..RunLayer::default()
        });

        assert!(resolve_docker_config(&settings.run).skip_clone);
        assert!(resolve_daytona_config(&settings.run).skip_clone);
    }

    #[test]
    fn runtime_docker_config_maps_environment_hints() {
        let settings = settings_from_run_layer(RunLayer {
            environment: Some(RunEnvironmentLayer {
                image: Some(EnvironmentImageLayer {
                    docker: Some("ubuntu:24.04".to_string()),
                    ..EnvironmentImageLayer::default()
                }),
                resources: Some(EnvironmentResourcesLayer {
                    cpu:    Some(4),
                    memory: Some("2GB".parse().unwrap()),
                    disk:   None,
                }),
                network: Some(EnvironmentNetworkLayer {
                    mode:  Some("block".to_string()),
                    allow: Vec::new(),
                }),
                env: StickyMap::from(HashMap::from([(
                    "NODE_ENV".to_string(),
                    InterpString::parse("test"),
                )])),
                ..RunEnvironmentLayer::default()
            }),
            ..RunLayer::default()
        });

        let config = resolve_docker_config(&settings.run);

        assert_eq!(config.image, "ubuntu:24.04");
        assert_eq!(config.cpu_quota, Some(400_000));
        assert_eq!(config.memory_limit, Some(2_000_000_000));
        assert_eq!(config.network_mode.as_deref(), Some("none"));
        assert_eq!(config.env_vars, vec!["NODE_ENV=test"]);
    }

    #[test]
    fn runtime_daytona_config_preserves_volume_mounts() {
        let settings = settings_from_run_layer(RunLayer {
            environment: Some(RunEnvironmentLayer {
                volumes: Some(vec![EnvironmentVolumeLayer {
                    id:         "vol_auth".to_string(),
                    mount_path: "/home/daytona/.config".to_string(),
                    subpath:    Some("agents".to_string()),
                }]),
                ..RunEnvironmentLayer::default()
            }),
            ..RunLayer::default()
        });

        let config = resolve_daytona_config(&settings.run);

        assert_eq!(config.volumes.len(), 1);
        assert_eq!(config.volumes[0].volume_id, "vol_auth");
        assert_eq!(config.volumes[0].mount_path, "/home/daytona/.config");
        assert_eq!(config.volumes[0].subpath.as_deref(), Some("agents"));
    }

    #[test]
    fn start_record_git_options_honor_disabled_run_branch() {
        let mut settings = WorkflowSettings::default();
        settings.run.run_branch.enabled = false;
        let start = fabro_types::StartRecord {
            start_time: Utc::now(),
            run_branch: Some("fabro/run/test".to_string()),
            base_sha:   Some("abc123".to_string()),
        };

        assert!(
            git_checkpoint_options_from_start(&settings, &fixtures::RUN_1, Some(start)).is_none()
        );
    }

    #[test]
    fn start_record_git_options_honor_disabled_meta_branch() {
        let mut settings = WorkflowSettings::default();
        settings.run.meta_branch.enabled = false;
        let start = fabro_types::StartRecord {
            start_time: Utc::now(),
            run_branch: Some("fabro/run/test".to_string()),
            base_sha:   Some("abc123".to_string()),
        };

        let git = git_checkpoint_options_from_start(&settings, &fixtures::RUN_1, Some(start))
            .expect("run branch should remain enabled");

        assert_eq!(git.run_branch.as_deref(), Some("fabro/run/test"));
        assert_eq!(git.base_sha.as_deref(), Some("abc123"));
        assert_eq!(git.meta_branch, None);
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
                title: None,
                automation: None,
                git: None,
                fork_source_ref: None,
                parent_id: None,
                provenance: None,
                configured_providers: Vec::new(),
                web_url: None,
            },
            storage_root.to_path_buf(),
            test_catalog(),
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
        let steering_hub = Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone()));
        StartServices {
            run_id: fixtures::RUN_1,
            cancel_token: CancellationToken::new(),
            emitter,
            interviewer: Arc::new(fabro_interview::AutoApproveInterviewer::engine()),
            steering_hub,
            run_store: store.open_run(&fixtures::RUN_1).await.unwrap().into(),
            event_sink: RunEventSink::store(store.open_run(&fixtures::RUN_1).await.unwrap()),
            artifact_sink: None,
            run_control: None,
            github_app: None,
            github_permissions: HashMap::new(),
            vault: None,
            catalog: test_catalog(),
            on_node: None,
            registry_override: Some(registry),
            fabro_run_tools: None,
        }
    }

    use crate::test_support::{mark_run_running, test_usage};

    async fn append_completed_stage(
        run_store: &fabro_store::RunDatabase,
        node_id: &str,
        timing: fabro_types::StageTiming,
        billing: Option<BilledModelUsage>,
    ) {
        crate::event::append_event(run_store, &fixtures::RUN_1, &Event::StageCompleted {
            node_id: node_id.to_string(),
            name: node_id.to_string(),
            index: 0,
            timing,
            status: StageOutcome::Succeeded.to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            billing,
            failure: None,
            notes: None,
            files_touched: Vec::new(),
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures: None,
            restart_failure_signatures: None,
            response: None,
            attempt: 1,
            max_attempts: 1,
        })
        .await
        .unwrap();
    }

    async fn wait_for_conclusion(
        run_store: &fabro_store::RunDatabase,
    ) -> crate::records::Conclusion {
        for _ in 0..50 {
            if let Some(conclusion) = run_store.state().await.unwrap().conclusion {
                return conclusion;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("timed out waiting for run conclusion");
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
                        status: "succeeded".to_string(),
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
                        diff_summary: None,
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
        assert_eq!(started.finalized.conclusion.status, StageOutcome::Succeeded);
    }

    #[tokio::test]
    async fn start_events_roll_up_outcome_active_timing() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let stage_timing = Arc::new(Mutex::new(None));
        let run_timing = Arc::new(Mutex::new(None));
        {
            let stage_timing = Arc::clone(&stage_timing);
            let run_timing = Arc::clone(&run_timing);
            emitter.on_event(move |event| match &event.body {
                EventBody::StageCompleted(props) if event.node_id.as_deref() == Some("work") => {
                    *stage_timing.lock().unwrap() = Some(props.timing);
                }
                EventBody::RunCompleted(props) => {
                    *run_timing.lock().unwrap() = Some(props.timing);
                }
                _ => {}
            });
        }

        let mut registry = test_registry();
        registry.register("timed", Box::new(TimedOutcomeHandler));
        let (_persisted, store) = persisted_workflow(TIMED_DOT, &storage_root).await;

        let started = start(
            &run_dir,
            test_start_services(&store, &run_dir, emitter, Arc::new(registry)).await,
        )
        .await
        .unwrap();

        let stage_timing = stage_timing
            .lock()
            .unwrap()
            .expect("work stage should emit stage.completed timing");
        assert_eq!(stage_timing.inference_time_ms, 100);
        assert_eq!(stage_timing.tool_time_ms, 50);
        assert_eq!(stage_timing.active_time_ms, 150);

        let run_timing = run_timing
            .lock()
            .unwrap()
            .expect("successful run should emit run.completed timing");
        assert_eq!(run_timing.inference_time_ms, 100);
        assert_eq!(run_timing.tool_time_ms, 50);
        assert_eq!(run_timing.active_time_ms, 150);
        assert_eq!(started.finalized.conclusion.timing.inference_time_ms, 100);
        assert_eq!(started.finalized.conclusion.timing.tool_time_ms, 50);
        assert_eq!(started.finalized.conclusion.timing.active_time_ms, 150);
    }

    #[tokio::test]
    async fn persist_terminal_engine_failure_uses_conclusion_timing_and_billing() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        mark_run_running(&run_store, &fixtures::RUN_1).await;
        append_completed_stage(
            &run_store,
            "implement",
            fabro_types::StageTiming::new(1_000, 200, 300),
            Some(test_usage("gpt-5.4", 100, 50)),
        )
        .await;
        append_completed_stage(
            &run_store,
            "review",
            fabro_types::StageTiming::new(500, 25, 75),
            None,
        )
        .await;
        let run_store_handle: RunStoreHandle = run_store.clone().into();
        let event_sink = RunEventSink::store(run_store.clone());

        persist_terminal_engine_failure(
            fixtures::RUN_1,
            &run_store_handle,
            &event_sink,
            &run_dir,
            &Error::engine("visit limit exceeded"),
            Duration::from_millis(9_999),
        )
        .await;

        let projection = run_store.state().await.unwrap();
        let conclusion = projection
            .conclusion
            .expect("run.failed should populate conclusion");
        assert_eq!(conclusion.timing.wall_time_ms, 9_999);
        assert_eq!(conclusion.timing.inference_time_ms, 225);
        assert_eq!(conclusion.timing.tool_time_ms, 375);
        assert_eq!(conclusion.timing.active_time_ms, 600);
        assert_eq!(
            conclusion
                .billing
                .as_ref()
                .map(|billing| billing.total_tokens),
            Some(150),
        );
    }

    #[tokio::test]
    async fn bootstrap_guard_failure_uses_conclusion_timing_and_billing() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        mark_run_running(&run_store, &fixtures::RUN_1).await;
        append_completed_stage(
            &run_store,
            "implement",
            fabro_types::StageTiming::new(1_000, 120, 80),
            Some(test_usage("gpt-5.4", 40, 10)),
        )
        .await;
        let run_store_handle: RunStoreHandle = run_store.clone().into();
        let event_sink = RunEventSink::store(run_store.clone());

        {
            let _guard = DetachedRunBootstrapGuard::arm(
                fixtures::RUN_1,
                run_store_handle,
                event_sink,
                CancellationToken::new(),
            );
        }

        let conclusion = wait_for_conclusion(&run_store).await;
        assert_eq!(conclusion.timing.inference_time_ms, 120);
        assert_eq!(conclusion.timing.tool_time_ms, 80);
        assert_eq!(conclusion.timing.active_time_ms, 200);
        assert_eq!(
            conclusion
                .billing
                .as_ref()
                .map(|billing| billing.total_tokens),
            Some(50),
        );
    }

    #[tokio::test]
    async fn completion_guard_failure_uses_conclusion_timing_and_billing() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let (_persisted, store) = persisted_workflow(MINIMAL_DOT, &storage_root).await;
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        mark_run_running(&run_store, &fixtures::RUN_1).await;
        append_completed_stage(
            &run_store,
            "implement",
            fabro_types::StageTiming::new(1_000, 70, 30),
            Some(test_usage("gpt-5.4", 20, 5)),
        )
        .await;
        let run_store_handle: RunStoreHandle = run_store.clone().into();
        let event_sink = RunEventSink::store(run_store.clone());

        {
            let _guard = DetachedRunCompletionGuard::arm(
                fixtures::RUN_1,
                run_store_handle,
                event_sink,
                CancellationToken::new(),
            );
        }

        let conclusion = wait_for_conclusion(&run_store).await;
        assert_eq!(conclusion.timing.inference_time_ms, 70);
        assert_eq!(conclusion.timing.tool_time_ms, 30);
        assert_eq!(conclusion.timing.active_time_ms, 100);
        assert_eq!(
            conclusion
                .billing
                .as_ref()
                .map(|billing| billing.total_tokens),
            Some(25),
        );
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

        assert_eq!(started.finalized.conclusion.status, StageOutcome::Succeeded);
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
            (
                ManifestPath::from_wire("workflow.fabro").unwrap(),
                BundledWorkflow {
                    path:   ManifestPath::from_wire("workflow.fabro").unwrap(),
                    source: r#"digraph Root {
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
                    config: None,
                    files:  HashMap::new(),
                },
            ),
            (
                ManifestPath::from_wire("children/review.fabro").unwrap(),
                BundledWorkflow {
                    path:   ManifestPath::from_wire("children/review.fabro").unwrap(),
                    source: r"digraph Review {
                        start [shape=Mdiamond]
                        exit [shape=Msquare]
                        start -> exit
                    }"
                    .to_string(),
                    config: None,
                    files:  HashMap::new(),
                },
            ),
        ]));

        crate::operations::create(
            &store,
            crate::operations::CreateRunInput {
                workflow: crate::operations::WorkflowInput::Bundled(
                    workflow_bundle
                        .workflow(&ManifestPath::from_wire("workflow.fabro").unwrap())
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
                workflow_path: Some(ManifestPath::from_wire("workflow.fabro").unwrap()),
                workflow_bundle: Some(workflow_bundle),
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_1),
                title: None,
                automation: None,
                git: None,
                fork_source_ref: None,
                parent_id: None,
                provenance: None,
                configured_providers: Vec::new(),
                web_url: None,
            },
            storage_root,
            test_catalog(),
        )
        .await
        .unwrap();

        let started = start(
            &run_dir,
            test_start_services(&store, &run_dir, emitter, registry).await,
        )
        .await
        .unwrap();

        assert_eq!(started.finalized.conclusion.status, StageOutcome::Succeeded);
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

        assert_eq!(started.finalized.conclusion.status, StageOutcome::Succeeded);
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
                diff_summary: None,
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
            status:               StageOutcome::Succeeded,
            timing:               fabro_types::RunTiming::wall_only(1),
            failure:              None,
            final_git_commit_sha: None,
            stages:               vec![],
            billing:              None,
            total_retries:        0,
            diff:                 fabro_types::RunDiff::default(),
        };
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        crate::event::append_event(&run_store, &fixtures::RUN_1, &Event::CheckpointCompleted {
            node_id: checkpoint.current_node.clone(),
            status: "succeeded".to_string(),
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
            diff_summary: None,
        })
        .await
        .unwrap();
        crate::event::append_event(&run_store, &fixtures::RUN_1, &Event::RunRunnable {
            source: RunRunnableSource::StartRequested,
            actor:  None,
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
            timing:               conclusion.timing,
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               crate::run_status::SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
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
