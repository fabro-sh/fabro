use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fabro_auth::{CredentialSource, VaultCredentialSource};
use fabro_interview::{AutoApproveInterviewer, Interviewer};
use fabro_llm::client::Client as LlmClient;
use fabro_mcp::config::McpServerSettings;
use fabro_model::{Catalog, ProviderId};
use fabro_sandbox::daytona::DaytonaConfig;
use fabro_sandbox::from_environment::{
    daytona_config_from_environment, docker_config_from_environment_with_secrets,
    local_working_directory_from_environment,
};
use fabro_sandbox::{DockerSandboxOptions, SandboxSpec};
use fabro_static::EnvVars;
#[cfg(test)]
use fabro_types::GitRunTarget;
use fabro_types::settings::run::{
    ApprovalMode, McpServerSettings as ResolvedMcpServerSettings, PullRequestSettings,
    ResolvedGithubIntegration, ResolvedMcpEntry, RunMode, RunNamespace as ResolvedRunSettings,
    RunPrepareSettings as ResolvedRunPrepareSettings,
};
use fabro_types::{
    ManifestPath, RunId, RunRunnableSource, RunSpec, RunTarget, SandboxProviderKind,
    TargetValidationError,
};
use fabro_util::error::collect_chain;
use fabro_vault::Vault;
use tokio::runtime::Handle;
use tokio::sync::RwLock as AsyncRwLock;
use tokio::{fs, time};
use tokio_util::sync::CancellationToken;

use crate::artifact_upload::ArtifactSink;
use crate::context::Context;
use crate::error::{self, Error};
use crate::event::{
    Emitter, Event, EventBody, RunEventLogger, RunEventPersistenceError, RunEventSink,
    RunNoticeLevel, append_event_to_sink,
};
use crate::handler::HandlerRegistry;
use crate::model_fallback::{ModelFallbackNotice, ResolvedModelFallbacks, resolve_model_fallbacks};
use crate::outcome::{Outcome, StageOutcome};
use crate::pipeline::{
    self, FinalizeOptions, Finalized, InitOptions, LlmSpec, Persisted, PublishOptions, ResumeState,
    SandboxEnvSpec, build_conclusion_from_store, classify_engine_result,
};
#[cfg(test)]
use crate::records::Checkpoint;
use crate::run_control::RunControlState;
use crate::run_materialization::resolve_run_model;
use crate::run_metadata::metadata_branch_name;
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions, SetupCommand};
use crate::run_status::{FailureReason, RunStatus};
use crate::runtime_store::RunStoreHandle;
use crate::services::FabroRunToolServices;
use crate::steering_hub::SteeringHub;
#[cfg(feature = "test-support")]
use crate::test_support as workflow_test_support;
use crate::workflow_bundle::{RunDefinition, WorkflowBundle};

struct RunSession {
    cancel_token:      CancellationToken,
    emitter:           Arc<Emitter>,
    sandbox:           SandboxSpec,
    llm:               LlmSpec,
    fallback_notices:  Vec<ModelFallbackNotice>,
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
    vault:             Arc<AsyncRwLock<Vault>>,
    catalog:           Arc<Catalog>,
    fabro_run_tools:   Option<FabroRunToolServices>,
}

struct ResolvedStartLlm {
    model:       String,
    provider_id: ProviderId,
    fallbacks:   ResolvedModelFallbacks,
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
    /// The resolved GitHub integration request (interpolated permissions
    /// plus declared additional repositories) to inject into the sandbox
    /// env. Empty when the github integration requests no token.
    pub github_integration: ResolvedGithubIntegration,
    pub vault:              Arc<AsyncRwLock<Vault>>,
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
        .await?;
        append_event_to_sink(
            &services.event_sink,
            &services.run_id,
            &Event::RunRunnable {
                source: RunRunnableSource::StartRequested,
                actor:  None,
            },
        )
        .await?;
    }

    Box::pin(execute_persisted_run(run_dir, None, services)).await
}

pub(super) async fn execute_persisted_run(
    run_dir: &Path,
    resume: Option<ResumeState>,
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
        let error = Error::from(err);
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
    let started = Box::pin(session.run(persisted, resume)).await;

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
        let rendered_error = collect_chain(&err).join(": ");
        tracing::error!(
            run_id = %run_id,
            event = "run.failed",
            error = %rendered_error,
            "Failed to append run.failed event",
        );
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

fn stop_for_run_event_persistence_failure(
    cancel_token: &CancellationToken,
    error: RunEventPersistenceError,
) -> Error {
    cancel_token.cancel();
    error.into()
}

/// Race a pipeline step against the first latched run-event persistence
/// failure. When the failure wins, the step future is dropped mid-flight and
/// the run token is cancelled.
async fn race_persistence<T>(
    logger: &RunEventLogger,
    cancel_token: &CancellationToken,
    step: impl Future<Output = T>,
) -> Result<T, Error> {
    tokio::select! {
        result = step => Ok(result),
        failure = logger.wait_for_failure() => {
            Err(stop_for_run_event_persistence_failure(cancel_token, failure))
        }
    }
}

async fn flush_or_stop(
    logger: &RunEventLogger,
    cancel_token: &CancellationToken,
) -> Result<(), Error> {
    logger
        .flush()
        .await
        .map_err(|failure| stop_for_run_event_persistence_failure(cancel_token, failure))
}

impl RunSession {
    async fn new(persisted: &Persisted, services: StartServices) -> Result<Self, Error> {
        let record = persisted.run_spec();
        let settings = &record.settings;
        let state = services
            .run_store
            .state()
            .await
            .map_err(|err| Error::engine(err.to_string()))?;
        let dry_run_clone_target = settings.run.execution.mode == RunMode::DryRun
            && matches!(
                record.target.as_ref(),
                Some(RunTarget::Git(_) | RunTarget::None {})
            );
        let git = (!dry_run_clone_target)
            .then(|| git_checkpoint_options_from_start(settings, &record.run_id, state.start))
            .flatten();
        let definition_blob = state.spec.definition_blob;
        let accepted_definition = match definition_blob {
            Some(blob_hash) => {
                Some(load_accepted_run_definition(&services.run_store, blob_hash).await?)
            }
            None => None,
        };
        let workflow_path = accepted_definition
            .as_ref()
            .map(|definition| definition.workflow_path.clone());
        let workflow_bundle =
            accepted_definition.map(|definition| Arc::new(definition.workflow_bundle()));

        let resolved = &settings.run;
        let configured_sandbox_provider = resolve_sandbox_provider(resolved);
        let sandbox_provider = configured_sandbox_provider.effective_for(resolved.execution.mode);
        let clone_source = if dry_run_clone_target {
            CloneSourceForRun {
                origin_url: None,
                branch:     None,
                tag:        None,
                commit_sha: None,
                skip_clone: true,
            }
        } else {
            clone_source_for_run(record)?
        };
        // Clone avoidance and repository identity are independent for Local
        // folder targets: their files are already present, but GitHub tokens
        // and pull-request publication still need the persisted origin. Only
        // an explicit empty target or a clone-target dry-run uses a repository-
        // free scratch workspace.
        let repository_free_workspace =
            dry_run_clone_target || matches!(record.target.as_ref(), Some(RunTarget::None {}));
        let runtime_origin_url = (!repository_free_workspace)
            .then(|| record.repo_origin_url().map(str::to_string))
            .flatten();
        let catalog = Arc::clone(&services.catalog);
        let configured =
            configured_providers_for_start(&services.vault, Arc::clone(&catalog)).await;
        #[cfg(feature = "test-support")]
        let configured = workflow_test_support::test_configured_provider_ids(
            catalog.as_ref(),
            configured,
            process_env_var("FABRO_TEST_ASSUME_LLM_READY")
                .is_some_and(|value| !matches!(value.as_str(), "" | "0" | "false" | "no")),
        );
        let llm = resolve_start_llm(catalog.as_ref(), &configured, resolved)?;
        let vault_guard = services.vault.read().await;
        // Token-only secrets lookup over the vault read guard, shared across
        // every run-boundary resolver. A missing or non-Token secret becomes
        // `None`, so resolution fails closed with a secret error.
        let secret_lookup = |name: &str| vault_token_lookup(&vault_guard, name);
        let mcp_servers = resolved
            .agent
            .mcps
            .iter()
            .map(|(key, entry)| match entry {
                ResolvedMcpEntry::Resolved(server) => runtime_mcp_server(server, secret_lookup),
                // References must be resolved to concrete servers before the run
                // spec is persisted (server-side run-preparation pass). Reaching
                // worker startup with an unresolved reference is an invariant
                // violation, so fail loudly rather than silently dropping it.
                ResolvedMcpEntry::Reference(reference) => {
                    let message = format!(
                        "unresolved MCP server reference `{key}` (id `{}`) reached worker \
                         startup; references must be resolved before the run spec is persisted",
                        reference.id
                    );
                    Err(Error::engine(message))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        if configured_sandbox_provider != SandboxProviderKind::Local
            && matches!(record.target, Some(RunTarget::Folder { .. }))
        {
            return Err(Error::engine(
                "persisted folder run targets require the Local sandbox provider",
            ));
        }
        if configured_sandbox_provider == SandboxProviderKind::Local {
            if let Some(target @ (RunTarget::Git(_) | RunTarget::None {})) = record.target.as_ref()
            {
                return Err(Error::engine(format!(
                    "persisted {} run targets require a clone-based sandbox provider",
                    target.kind_name()
                )));
            }
        }
        let sandbox = match sandbox_provider {
            SandboxProviderKind::Local if dry_run_clone_target => SandboxSpec::Local {
                working_directory: dry_run_workspace_for_target(persisted).await?,
            },
            SandboxProviderKind::Local => match record.target.as_ref() {
                Some(target @ (RunTarget::Git(_) | RunTarget::None {})) => {
                    return Err(Error::engine(format!(
                        "persisted {} run targets require a clone-based sandbox provider",
                        target.kind_name()
                    )));
                }
                Some(RunTarget::Folder { path }) => SandboxSpec::Local {
                    working_directory: folder_working_directory_from_record(record, path).await?,
                },
                None => {
                    let working_directory = local_working_directory_from_environment(
                        &resolved.environment,
                        record.source_directory.as_deref().map(Path::new),
                    )
                    .map_err(|err| {
                        Error::engine_with_source(
                            "Failed to resolve local environment working directory",
                            err,
                        )
                    })?;
                    SandboxSpec::Local { working_directory }
                }
            },
            SandboxProviderKind::Docker => {
                let mut config = resolve_docker_config(resolved, secret_lookup)?;
                config.skip_clone |= clone_source.skip_clone;
                SandboxSpec::Docker {
                    config,
                    github_app: services.github_app.clone(),
                    run_id: Some(record.run_id),
                    clone_origin_url: clone_source.origin_url,
                    clone_branch: clone_source.branch,
                    clone_tag: clone_source.tag,
                    clone_commit_sha: clone_source.commit_sha,
                }
            }
            SandboxProviderKind::Daytona => {
                let api_key = vault_guard
                    .get(EnvVars::DAYTONA_API_KEY)
                    .map(str::to_string);
                let mut config = resolve_daytona_config(resolved);
                config.skip_clone |= clone_source.skip_clone;
                SandboxSpec::Daytona {
                    config: Box::new(config),
                    github_app: services.github_app.clone(),
                    run_id: Some(record.run_id),
                    clone_origin_url: clone_source.origin_url,
                    clone_branch: clone_source.branch,
                    clone_tag: clone_source.tag,
                    clone_commit_sha: clone_source.commit_sha,
                    api_key,
                }
            }
        };

        let toml_env = resolved
            .environment
            .resolve_env(secret_lookup)
            .map_err(|err| Error::engine_with_source("failed to resolve run environment", err))?;
        let github_integration = services
            .github_integration
            .is_token_requested()
            .then(|| services.github_integration.clone());
        let sandbox_env = SandboxEnvSpec {
            toml_env,
            github_integration,
            origin_url: runtime_origin_url.clone(),
        };

        let interviewer: Arc<dyn Interviewer> = if resolved.execution.approval == ApprovalMode::Auto
        {
            Arc::new(AutoApproveInterviewer::engine())
        } else {
            services.interviewer
        };

        let pr_config = resolved.pull_request.clone();
        let setup_commands = runtime_setup_commands(&resolved.prepare, secret_lookup)?;
        drop(vault_guard);

        Ok(Self {
            cancel_token: services.cancel_token,
            emitter: services.emitter,
            event_sink: services.event_sink,
            run_control: services.run_control,
            sandbox,
            llm: LlmSpec {
                model: llm.model.clone(),
                provider_id: llm.provider_id.clone(),
                fallbacks: llm.fallbacks.policy,
                mcp_servers,
                skill_dirs: resolved.agent.skill_dirs.clone(),
                model_controls: resolved.model.controls.clone(),
                dry_run: resolved.execution.mode == RunMode::DryRun,
            },
            fallback_notices: llm.fallbacks.notices,
            interviewer,
            steering_hub: services.steering_hub,
            on_node: services.on_node,
            lifecycle: LifecycleOptions {
                setup_commands,
                setup_command_timeout_ms: resolved.prepare.timeout_ms,
            },
            hooks: fabro_hooks::HookSettings {
                hooks: resolved.hooks.clone(),
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
            pr_origin_url: runtime_origin_url,
            pr_model: llm.model,
            workflow_path,
            workflow_bundle,
            vault: services.vault,
            catalog,
            fabro_run_tools: services.fabro_run_tools,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CloneSourceForRun {
    origin_url: Option<String>,
    branch:     Option<String>,
    tag:        Option<String>,
    commit_sha: Option<String>,
    /// The target asked for an empty workspace, so the provider must not
    /// clone even when it would otherwise inherit an origin.
    skip_clone: bool,
}

async fn folder_working_directory_from_record(
    record: &RunSpec,
    target_path: &str,
) -> Result<PathBuf, Error> {
    let source_directory = record.source_directory.as_deref().ok_or_else(|| {
        Error::engine("persisted folder run target is missing its source-directory projection")
    })?;
    if source_directory != target_path {
        return Err(Error::engine(
            "persisted folder run target disagrees with its source-directory projection",
        ));
    }

    // The persisted path was canonical at admission, so it is absolute and
    // symlink-free. Re-canonicalizing detects any redirection since then.
    let canonical = fs::canonicalize(target_path).await.map_err(|source| {
        Error::engine_with_source(
            "persisted folder run target path could not be canonicalized",
            source,
        )
    })?;
    if canonical.to_str() != Some(target_path) {
        return Err(Error::engine(
            "persisted folder run target path is no longer canonical",
        ));
    }

    let metadata = fs::metadata(&canonical).await.map_err(|source| {
        Error::engine_with_source(
            "persisted folder run target path could not be inspected",
            source,
        )
    })?;
    if !metadata.is_dir() {
        return Err(Error::engine(
            "persisted folder run target path is not a directory",
        ));
    }

    Ok(canonical)
}

async fn dry_run_workspace_for_target(persisted: &Persisted) -> Result<PathBuf, Error> {
    let workspace = persisted.run_dir().join("dry-run-workspace");
    fs::create_dir_all(&workspace).await.map_err(|source| {
        Error::engine_with_source("failed to create dry-run target workspace", source)
    })?;
    fs::canonicalize(&workspace).await.map_err(|source| {
        Error::engine_with_source("failed to canonicalize dry-run target workspace", source)
    })
}

fn clone_source_for_run(record: &RunSpec) -> Result<CloneSourceForRun, Error> {
    let Some(target) = &record.target else {
        return Ok(CloneSourceForRun {
            origin_url: record.repo_origin_url().map(str::to_string),
            branch:     record.base_branch().map(str::to_string),
            tag:        None,
            commit_sha: None,
            skip_clone: false,
        });
    };

    // The Git-target grammar is owned by `RunTarget::validate` in fabro-types;
    // admission accepts targets through the same rules, and this start path
    // re-derives the clone source from the persisted target alone. The
    // persisted `git` projection is display metadata, never a clone input, so
    // writers cannot break starts by letting the pair drift.
    let validated = target.clone().validate().map_err(|error| {
        Error::engine(match error {
            TargetValidationError::Repository => {
                "persisted Git run target has an invalid repository slug"
            }
            TargetValidationError::Branch => "persisted Git run target has an invalid branch",
            TargetValidationError::Tag => "persisted Git run target has an invalid tag",
            TargetValidationError::Sha => "persisted Git run target has an invalid SHA",
        })
    })?;
    // A target with no Git projection (`none` or `folder`) supplies no clone
    // source. Folder targets only reach the Local provider, where `skip_clone`
    // is unused.
    Ok(match (validated.target, validated.git) {
        (RunTarget::Git(target), Some(git)) => CloneSourceForRun {
            origin_url: Some(git.origin_url),
            branch:     Some(target.branch),
            tag:        target.tag,
            commit_sha: git.sha,
            skip_clone: false,
        },
        _ => CloneSourceForRun {
            origin_url: None,
            branch:     None,
            tag:        None,
            commit_sha: None,
            skip_clone: true,
        },
    })
}

async fn configured_providers_for_start(
    vault: &Arc<AsyncRwLock<Vault>>,
    catalog: Arc<Catalog>,
) -> Vec<ProviderId> {
    let source: Arc<dyn CredentialSource> = Arc::new(VaultCredentialSource::with_env_lookup(
        Arc::clone(vault),
        process_env_var,
    ));
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
    reason = "Run startup reads process env only for explicit provider credential refs and test mode."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn vault_token_lookup(vault: &Vault, name: &str) -> Option<String> {
    fabro_auth::vault_get_token(vault, name).ok().flatten()
}

async fn load_accepted_run_definition(
    run_store: &RunStoreHandle,
    blob_hash: fabro_types::BlobHash,
) -> Result<RunDefinition, Error> {
    let bytes = run_store
        .read_blob(&blob_hash)
        .await
        .map_err(|err| Error::engine(err.to_string()))?
        .ok_or_else(|| {
            Error::engine(format!(
                "run definition blob is missing from the run store: {blob_hash}"
            ))
        })?;
    serde_json::from_slice(&bytes).map_err(|err| Error::Parse(err.to_string()))
}

fn resolve_sandbox_provider(settings: &ResolvedRunSettings) -> SandboxProviderKind {
    SandboxProviderKind::from(settings.environment.provider)
}

fn resolve_daytona_config(settings: &ResolvedRunSettings) -> DaytonaConfig {
    daytona_config_from_environment(&settings.environment, &settings.clone)
}

fn resolve_docker_config(
    settings: &ResolvedRunSettings,
    secrets_lookup: impl FnMut(&str) -> Option<String>,
) -> Result<DockerSandboxOptions, Error> {
    docker_config_from_environment_with_secrets(
        &settings.environment,
        &settings.clone,
        secrets_lookup,
    )
    .map_err(|err| Error::engine_with_source("failed to resolve Docker environment config", err))
}

fn resolve_start_llm(
    catalog: &Catalog,
    configured: &[ProviderId],
    settings: &ResolvedRunSettings,
) -> Result<ResolvedStartLlm, Error> {
    let eligible = configured.iter().cloned().collect::<HashSet<_>>();
    let (model, provider_id) = resolve_run_model(
        catalog,
        &eligible,
        settings.model.name.as_deref(),
        settings.model.provider.as_deref(),
        false,
    )?;
    let fallbacks = resolve_model_fallbacks(catalog, configured, &settings.model.fallbacks)?;

    Ok(ResolvedStartLlm {
        model,
        provider_id,
        fallbacks,
    })
}

/// Build the launch-time MCP config from resolved settings. Secret tokens in
/// the transport (`command`/`url`/`env`/`headers`) resolve from the vault at
/// the run boundary. Unsupported tokens fail.
///
/// The resolution itself lives on the type
/// ([`McpServerSettings::resolve_transport_secrets`]) so `fabro run` (here) and
/// `fabro exec` share one resolver; this wrapper just adds the server name to
/// the error. MCP transport strings are carried in source form out of the
/// config resolve layer so `fabro validate` stays portable. A missing or
/// non-token secret is a hard error.
fn runtime_mcp_server(
    settings: &ResolvedMcpServerSettings,
    secrets_lookup: impl FnMut(&str) -> Option<String>,
) -> Result<McpServerSettings, Error> {
    settings
        .resolve_transport_secrets(secrets_lookup)
        .map_err(|err| {
            Error::engine_with_source(
                format!("failed to resolve MCP server {:?}", settings.name),
                err,
            )
        })
}

/// Build the launch-time setup (prepare) commands from resolved settings.
/// Secret tokens in each step's command and per-step env resolve from the vault
/// at the run boundary. Unsupported tokens fail.
///
/// The resolution itself lives on the type
/// ([`ResolvedRunPrepareSettings::resolve_step_secrets`]) so prepare-step
/// resolution shares one resolver with the rest of the run-boundary
/// interpolation. Prepare-step commands and env are carried in source form out
/// of the config resolve layer so `fabro validate` stays portable. A missing or
/// non-token secret is a hard error.
fn runtime_setup_commands(
    prepare: &ResolvedRunPrepareSettings,
    secrets_lookup: impl FnMut(&str) -> Option<String>,
) -> Result<Vec<SetupCommand>, Error> {
    let resolved = prepare
        .resolve_step_secrets(secrets_lookup)
        .map_err(|err| Error::engine_with_source("failed to resolve prepare step", err))?;
    Ok(resolved
        .steps
        .into_iter()
        .map(|step| SetupCommand {
            // Flatten the runnable part into the shell string AFTER env
            // resolution: an argv `command` is shell-quoted per resolved
            // element here so an interpolated value stays a single token; a
            // `script` is kept verbatim.
            command: step.to_shell_command(),
            env:     step.env,
        })
        .collect())
}

impl RunSession {
    /// Shared engine: initialize, execute, conclude, publish, finalize.
    async fn run(
        self,
        persisted: Persisted,
        resume: Option<ResumeState>,
    ) -> Result<Started, Error> {
        let on_node = self.on_node.clone();
        let run_cancel_token = self.cancel_token.clone();

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
        // Emit after the logger is registered so the notices reach the run
        // store, and before `run.started` so they read as launch-time context.
        for notice in &self.fallback_notices {
            self.emitter
                .notice(notice.level(), notice.code(), notice.message());
        }

        let init_options = InitOptions {
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
            resume,
            seed_context: self.seed_context,
            fabro_run_tools: self.fabro_run_tools,
        };
        let mut initialized = match race_persistence(
            &store_progress_logger,
            &run_cancel_token,
            Box::pin(pipeline::initialize(persisted, init_options)),
        )
        .await?
        {
            Ok(initialized) => initialized,
            Err(err) => {
                flush_or_stop(&store_progress_logger, &run_cancel_token).await?;
                return Err(err);
            }
        };
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

        flush_or_stop(&store_progress_logger, &run_cancel_token).await?;

        let executed = race_persistence(
            &store_progress_logger,
            &run_cancel_token,
            Box::pin(pipeline::execute(initialized)),
        )
        .await?;
        flush_or_stop(&store_progress_logger, &run_cancel_token).await?;
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
        let publish_opts = PublishOptions {
            pr_config:  self.pr_config,
            github_app: self.pr_github_app,
            origin_url: self.pr_origin_url,
            model:      self.pr_model,
        };

        let concluding = race_persistence(
            &store_progress_logger,
            &run_cancel_token,
            Box::pin(async {
                let concluded = Box::pin(pipeline::conclude(executed, &finalize_opts)).await?;
                let published = Box::pin(pipeline::publish(concluded, &publish_opts)).await;
                Box::pin(pipeline::finalize(published, &finalize_opts)).await
            }),
        )
        .await?;
        let finalized = match concluding {
            Ok(finalized) => finalized,
            Err(err) => {
                self.steering_hub.drain_pending_at_run_end();
                flush_or_stop(&store_progress_logger, &run_cancel_token).await?;
                return Err(err);
            }
        };
        // Emit `agent.steer.dropped { reason: run_ended }` for any
        // unconsumed pending steers on the success path, then flush. The
        // scopeguard above re-runs as a no-op (drain is idempotent on an
        // already-empty buffer) on the way out of scope.
        self.steering_hub.drain_pending_at_run_end();
        flush_or_stop(&store_progress_logger, &run_cancel_token).await?;

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
const DETACHED_COMPLETION_GUARD_TERMINAL_GRACE: Duration = Duration::from_millis(25);

async fn run_store_reaches_terminal(run_store: &RunStoreHandle, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if run_store
            .state()
            .await
            .is_ok_and(|state| state.status.is_terminal())
        {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        time::sleep(Duration::from_millis(10)).await;
    }
}

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
                if run_store_reaches_terminal(&run_store, DETACHED_COMPLETION_GUARD_TERMINAL_GRACE)
                    .await
                {
                    return;
                }
                emit_workflow_run_failed(
                    run_id,
                    &run_store,
                    &event_sink,
                    &Error::engine(message.to_string()),
                    reason,
                    0,
                )
                .await;
                if let Err(err) = append_event_to_sink(&event_sink, &run_id, &Event::RunNotice {
                    level:            RunNoticeLevel::Error,
                    code:             code.to_string(),
                    message:          message.to_string(),
                    exec_output_tail: None,
                })
                .await
                {
                    let rendered_error = collect_chain(&err).join(": ");
                    tracing::warn!(
                        error = %rendered_error,
                        "Failed to append detached completion notice",
                    );
                }
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
        let rendered_error = collect_chain(&err).join(": ");
        tracing::warn!(
            error = %rendered_error,
            "Failed to append detached failure notice",
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chrono::Utc;
    use fabro_config::{
        EnvironmentImageLayer, EnvironmentNetworkLayer, EnvironmentResourcesLayer, RunCloneLayer,
        RunEnvironmentLayer, RunExecutionLayer, RunLayer, StickyMap, WorkflowSettingsBuilder,
    };
    use fabro_sandbox::test_support::MockSandbox;
    use fabro_store::Database;
    use fabro_types::settings::InterpString;
    use fabro_types::settings::run::{
        EnvironmentProvider, McpTransport as ResolvedMcpTransport, PreparedStep, PreparedStepRun,
        RunMode, RunPrepareSettings,
    };
    use fabro_types::{
        BilledModelUsage, GitContext, ManifestPath, RunTarget, StageTiming, WorkflowSettings,
        fixtures, test_support,
    };
    use fabro_vault::SecretType;
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

    const BLOCKING_DOT: &str = r#"digraph Test {
        graph [goal="Wait forever"]
        start [shape=Mdiamond]
        block [type="blocking"]
        exit  [shape=Msquare]
        start -> block
        block -> exit
    }"#;

    struct TimedOutcomeHandler;

    struct BlockingHandler;

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

    #[async_trait::async_trait]
    impl Handler for BlockingHandler {
        async fn execute(
            &self,
            _node: &fabro_graphviz::graph::Node,
            _context: &Context,
            _graph: &fabro_graphviz::graph::Graph,
            _run_dir: &Path,
            _services: &EngineServices,
        ) -> Result<Outcome, Error> {
            std::future::pending().await
        }
    }

    fn memory_store() -> Arc<Database> {
        Arc::new(fabro_store::test_support::test_database(
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
            .server_manifest_defaults(
                RunLayer::default(),
                fabro_environment::seeded_catalog_layer(),
            )
            .run_overrides(run)
            .build()
            .expect("settings should resolve")
    }

    fn test_catalog() -> Arc<Catalog> {
        Arc::new(Catalog::from_builtin().expect("default catalog should build"))
    }

    fn test_provider_ids() -> Vec<ProviderId> {
        Catalog::builtin().all_provider_ids().into_iter().collect()
    }

    fn portable_model_catalog() -> Catalog {
        let settings: fabro_model::catalog::LlmCatalogSettings = toml::from_str(
            r#"
[providers.openai]
display_name = "OpenAI"
adapter = "openai"
agent_profile = "openai"
priority = 90

[providers.openai.models."gpt-5.6-sol"]
display_name = "GPT-5.6 Sol"
family = "gpt-5"
aliases = ["gpt-56-sol"]
default = true

[providers.openai.models."gpt-5.6-sol".limits]
context_window = 1000

[providers.openai.models."gpt-5.6-sol".features]
tools = true
vision = false
reasoning = false

[providers.openai.models."gpt-5.4-mini"]
display_name = "GPT-5.4 Mini"
family = "gpt-5"
aliases = ["mini"]

[providers.openai.models."gpt-5.4-mini".limits]
context_window = 1000

[providers.openai.models."gpt-5.4-mini".features]
tools = true
vision = false
reasoning = false

[providers.openrouter]
display_name = "OpenRouter"
adapter = "openai_compatible"
agent_profile = "openai"
priority = 25

[providers.openrouter.models."gpt-5.6-sol"]
api_id = "openai/gpt-5.6-sol"
display_name = "GPT-5.6 Sol (via OpenRouter)"
family = "gpt-5"
aliases = ["gpt-56-sol"]
default = true

[providers.openrouter.models."gpt-5.6-sol".limits]
context_window = 1000

[providers.openrouter.models."gpt-5.6-sol".features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        Catalog::from_settings(&settings).unwrap()
    }

    #[test]
    fn materialized_provider_pin_is_not_reselected_when_readiness_changes() {
        let catalog = portable_model_catalog();
        let mut settings = ResolvedRunSettings::default();
        settings.model.name = Some("gpt-5.6-sol".to_string());
        settings.model.provider = Some("openai".to_string());

        let Err(error) = resolve_start_llm(&catalog, &[ProviderId::new("openrouter")], &settings)
        else {
            panic!("materialized provider pin should remain fixed");
        };

        assert!(matches!(
            error,
            Error::ModelSelection(fabro_model::ModelSelectionError::ProviderUnavailable {
                provider
            }) if provider == ProviderId::openai()
        ));
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
        settings.model.name = Some("ac".to_string());

        let resolved = resolve_start_llm(&catalog, &[ProviderId::new("acme")], &settings).unwrap();

        assert_eq!(resolved.model, "acme-claude");
        assert_eq!(resolved.provider_id, ProviderId::new("acme"));
    }

    #[test]
    fn runtime_clone_config_uses_run_level_clone_policy() {
        let settings = settings_from_run_layer(RunLayer {
            clone: Some(RunCloneLayer {
                enabled: Some(false),
                depth:   Some(1),
            }),
            ..RunLayer::default()
        });

        assert!(
            resolve_docker_config(&settings.run, |_| None)
                .unwrap()
                .skip_clone
        );
        assert!(resolve_daytona_config(&settings.run).skip_clone);
        assert_eq!(resolve_daytona_config(&settings.run).clone_depth, Some(1));
        assert_eq!(
            resolve_docker_config(&settings.run, |_| None)
                .unwrap()
                .clone_depth,
            Some(1)
        );
    }

    #[test]
    fn zero_clone_depth_requests_full_history_from_clone_providers() {
        let settings = settings_from_run_layer(RunLayer {
            clone: Some(RunCloneLayer {
                enabled: None,
                depth:   Some(0),
            }),
            ..RunLayer::default()
        });

        assert_eq!(resolve_daytona_config(&settings.run).clone_depth, None);
        assert_eq!(
            resolve_docker_config(&settings.run, |_| None)
                .unwrap()
                .clone_depth,
            None
        );
    }

    #[test]
    fn clone_providers_default_to_depth_100() {
        let settings = settings_from_run_layer(RunLayer::default());

        assert_eq!(resolve_daytona_config(&settings.run).clone_depth, Some(100));
        assert_eq!(
            resolve_docker_config(&settings.run, |_| None)
                .unwrap()
                .clone_depth,
            Some(100)
        );
    }

    #[test]
    fn runtime_mcp_server_wraps_resolve_error_source() {
        let settings = ResolvedMcpServerSettings {
            name: "gemini".to_string(),
            transport: ResolvedMcpTransport::Stdio {
                command: vec!["python".to_string()],
                env:     HashMap::from([(
                    "GEMINI_API_KEY".to_string(),
                    "{{ env.GEMINI_API_KEY }}".to_string(),
                )]),
            },
            ..ResolvedMcpServerSettings::default()
        };

        let err = runtime_mcp_server(&settings, |_| None).unwrap_err();

        assert_eq!(
            err.to_string(),
            "Engine error: failed to resolve MCP server \"gemini\""
        );
        let causes = err.causes();
        assert_eq!(causes.len(), 1);
        assert!(causes[0].contains("GEMINI_API_KEY"));
    }

    #[test]
    fn runtime_setup_command_env_resolves_secret_from_vault() {
        let vault = token_vault("DEPLOY_TOKEN", "vault-token");
        let prepare = prepare_with_step(script_step(
            "echo ready",
            HashMap::from([(
                "DEPLOY_TOKEN".to_string(),
                "{{ secrets.DEPLOY_TOKEN }}".to_string(),
            )]),
        ));

        let commands = runtime_setup_commands(&prepare, vault_secret_lookup(&vault)).unwrap();

        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].env.get("DEPLOY_TOKEN").map(String::as_str),
            Some("vault-token")
        );
    }

    #[test]
    fn runtime_setup_command_secret_argv_is_resolved_before_shell_quoting() {
        let malicious = "x'; touch PWNED; echo '";
        let vault = token_vault("USER_INPUT", malicious);
        let prepare = prepare_with_step(command_step(
            &["echo", "{{ secrets.USER_INPUT }}"],
            HashMap::new(),
        ));

        let commands = runtime_setup_commands(&prepare, vault_secret_lookup(&vault)).unwrap();
        let tokens =
            shlex::split(&commands[0].command).expect("resolved command should remain valid shell");

        assert_eq!(tokens, vec!["echo".to_string(), malicious.to_string()]);
        assert_eq!(
            tokens.len(),
            2,
            "injected shell syntax leaked extra tokens: {}",
            commands[0].command
        );
    }

    #[test]
    fn runtime_mcp_server_env_resolves_secret_from_vault() {
        let vault = token_vault("MCP_TOKEN", "vault-token");
        let settings = ResolvedMcpServerSettings {
            name: "vaulted".to_string(),
            transport: ResolvedMcpTransport::Stdio {
                command: vec!["mcp-server".to_string()],
                env:     HashMap::from([(
                    "MCP_TOKEN".to_string(),
                    "{{ secrets.MCP_TOKEN }}".to_string(),
                )]),
            },
            ..ResolvedMcpServerSettings::default()
        };

        let resolved = runtime_mcp_server(&settings, vault_secret_lookup(&vault)).unwrap();

        let ResolvedMcpTransport::Stdio { env, .. } = resolved.transport else {
            panic!("expected stdio transport");
        };
        assert_eq!(
            env.get("MCP_TOKEN").map(String::as_str),
            Some("vault-token")
        );
    }

    #[test]
    fn runtime_setup_command_missing_secret_fails_closed() {
        let vault = temp_vault(&[]);
        let prepare = prepare_with_step(command_step(
            &["deploy", "{{ secrets.DEPLOY_TOKEN }}"],
            HashMap::new(),
        ));

        let Err(err) = runtime_setup_commands(&prepare, vault_secret_lookup(&vault)) else {
            panic!("missing secret should fail setup command resolution");
        };

        assert_eq!(
            err.to_string(),
            "Engine error: failed to resolve prepare step"
        );
        let causes = err.causes();
        assert_eq!(causes.len(), 1);
        assert!(causes[0].contains("DEPLOY_TOKEN"));
    }

    #[test]
    fn runtime_setup_command_oauth_secret_fails_closed() {
        let vault = temp_vault(&[("DEPLOY_TOKEN", "{}", SecretType::Oauth)]);
        let prepare = prepare_with_step(script_step(
            "echo ready",
            HashMap::from([(
                "DEPLOY_TOKEN".to_string(),
                "{{ secrets.DEPLOY_TOKEN }}".to_string(),
            )]),
        ));

        let Err(err) = runtime_setup_commands(&prepare, vault_secret_lookup(&vault)) else {
            panic!("OAuth secret should fail setup command resolution");
        };

        assert_eq!(
            err.to_string(),
            "Engine error: failed to resolve prepare step"
        );
        assert!(err.causes()[0].contains("DEPLOY_TOKEN"));
    }

    #[test]
    fn runtime_setup_command_file_secret_fails_closed() {
        let vault = temp_vault(&[(EnvVars::GITHUB_APP_PRIVATE_KEY, "pem", SecretType::File)]);
        let prepare = prepare_with_step(script_step(
            "echo ready",
            HashMap::from([(
                "GITHUB_APP_PRIVATE_KEY".to_string(),
                "{{ secrets.GITHUB_APP_PRIVATE_KEY }}".to_string(),
            )]),
        ));

        let Err(err) = runtime_setup_commands(&prepare, vault_secret_lookup(&vault)) else {
            panic!("file secret should fail setup command resolution");
        };

        assert_eq!(
            err.to_string(),
            "Engine error: failed to resolve prepare step"
        );
        assert!(err.causes()[0].contains("GITHUB_APP_PRIVATE_KEY"));
    }

    #[tokio::test]
    async fn run_session_new_resolves_secret_tokens_from_vault_at_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let mut settings = settings_from_run_layer(RunLayer {
            execution: Some(RunExecutionLayer {
                mode: Some(RunMode::DryRun),
                ..RunExecutionLayer::default()
            }),
            ..RunLayer::default()
        });
        settings.run.environment.env.insert(
            "API_TOKEN".to_string(),
            InterpString::parse("{{ secrets.DEPLOY_TOKEN }}"),
        );
        settings.run.prepare = prepare_with_step(command_step(
            &["deploy", "{{ secrets.DEPLOY_TOKEN }}"],
            HashMap::from([(
                "DEPLOY_TOKEN".to_string(),
                "{{ secrets.DEPLOY_TOKEN }}".to_string(),
            )]),
        ));
        settings.run.agent.mcps.insert(
            "vaulted".to_string(),
            ResolvedMcpEntry::Resolved(ResolvedMcpServerSettings {
                name: "vaulted".to_string(),
                transport: ResolvedMcpTransport::Stdio {
                    command: vec!["mcp-server".to_string()],
                    env:     HashMap::from([(
                        "MCP_TOKEN".to_string(),
                        "{{ secrets.DEPLOY_TOKEN }}".to_string(),
                    )]),
                },
                ..ResolvedMcpServerSettings::default()
            }),
        );
        let (persisted, store) =
            persisted_workflow_with_settings(MINIMAL_DOT, &storage_root, settings).await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());
        let vault = Arc::new(AsyncRwLock::new(start_vault(&[(
            "DEPLOY_TOKEN",
            "vault-token",
            SecretType::Token,
        )])));

        let session = RunSession::new(&persisted, StartServices {
            vault,
            ..test_start_services(&store, &storage_root, emitter, registry).await
        })
        .await
        .unwrap();

        assert_eq!(
            session
                .sandbox_env
                .toml_env
                .get("API_TOKEN")
                .map(String::as_str),
            Some("vault-token")
        );
        assert_eq!(
            session.lifecycle.setup_commands[0]
                .env
                .get("DEPLOY_TOKEN")
                .map(String::as_str),
            Some("vault-token")
        );
        let setup_command = &session.lifecycle.setup_commands[0].command;
        assert!(!setup_command.contains("{{ secrets.DEPLOY_TOKEN }}"));
        assert_eq!(
            shlex::split(setup_command).expect("setup command should be valid shell"),
            vec!["deploy".to_string(), "vault-token".to_string()]
        );
        let ResolvedMcpTransport::Stdio { env, .. } = &session.llm.mcp_servers[0].transport else {
            panic!("expected stdio MCP transport");
        };
        assert_eq!(
            env.get("MCP_TOKEN").map(String::as_str),
            Some("vault-token")
        );
    }

    /// `[run.agent] skill_dirs` has to reach the LLM spec, which is what hands
    /// the directories to every agent session the run creates.
    #[tokio::test]
    async fn run_session_new_carries_run_agent_skill_dirs_into_the_llm_spec() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let mut settings = settings_from_run_layer(RunLayer {
            execution: Some(RunExecutionLayer {
                mode: Some(RunMode::DryRun),
                ..RunExecutionLayer::default()
            }),
            ..RunLayer::default()
        });
        settings.run.agent.skill_dirs = vec![
            ".agents/skills".to_string(),
            "/opt/shared/skills".to_string(),
        ];
        let (persisted, store) =
            persisted_workflow_with_settings(MINIMAL_DOT, &storage_root, settings).await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let session = RunSession::new(
            &persisted,
            test_start_services(&store, &storage_root, emitter, registry).await,
        )
        .await
        .unwrap();

        assert_eq!(session.llm.skill_dirs, vec![
            ".agents/skills".to_string(),
            "/opt/shared/skills".to_string(),
        ]);
    }

    #[tokio::test]
    async fn run_session_new_missing_secret_fails_startup() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let mut settings = settings_from_run_layer(RunLayer {
            execution: Some(RunExecutionLayer {
                mode: Some(RunMode::DryRun),
                ..RunExecutionLayer::default()
            }),
            ..RunLayer::default()
        });
        settings.run.prepare = prepare_with_step(command_step(
            &["deploy", "{{ secrets.DEPLOY_TOKEN }}"],
            HashMap::new(),
        ));
        let (persisted, store) =
            persisted_workflow_with_settings(MINIMAL_DOT, &storage_root, settings).await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());
        let vault = Arc::new(AsyncRwLock::new(start_vault(&[])));

        let Err(err) = RunSession::new(&persisted, StartServices {
            vault,
            ..test_start_services(&store, &storage_root, emitter, registry).await
        })
        .await
        else {
            panic!("missing secret should fail run startup");
        };

        assert_eq!(
            err.to_string(),
            "Engine error: failed to resolve prepare step"
        );
        assert!(err.causes()[0].contains("DEPLOY_TOKEN"));
    }

    #[tokio::test]
    async fn run_session_new_none_target_forces_empty_docker_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let mut settings = settings_from_run_layer(RunLayer {
            clone: Some(RunCloneLayer {
                enabled: Some(true),
                depth:   None,
            }),
            ..RunLayer::default()
        });
        settings.run.environment.provider = EnvironmentProvider::Docker;
        settings.run.environment.image.docker = Some("buildpack-deps:noble".to_string());
        let (persisted, store) = persisted_workflow_with_settings_and_target(
            MINIMAL_DOT,
            &storage_root,
            settings,
            Some(RunTarget::None {}),
        )
        .await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let session = RunSession::new(
            &persisted,
            test_start_services(&store, &storage_root, emitter, registry).await,
        )
        .await
        .unwrap();

        let RunSession {
            sandbox,
            sandbox_env,
            pr_origin_url,
            ..
        } = session;
        let runtime = sandbox
            .to_run_sandbox_instance(&MockSandbox::linux(), fixtures::RUN_1)
            .runtime;
        assert_eq!(runtime.repo_cloned, Some(false));
        assert_eq!(runtime.clone_origin_url, None);
        assert_eq!(runtime.clone_branch, None);
        assert_eq!(runtime.primary_repo_path, None);
        assert_eq!(runtime.primary_repo_link, None);
        let SandboxSpec::Docker {
            config,
            clone_origin_url,
            clone_branch,
            clone_commit_sha,
            ..
        } = sandbox
        else {
            panic!("none target should retain the selected Docker provider");
        };
        assert!(config.skip_clone);
        assert_eq!(clone_origin_url, None);
        assert_eq!(clone_branch, None);
        assert_eq!(clone_commit_sha, None);
        assert_eq!(sandbox_env.origin_url, None);
        assert_eq!(pr_origin_url, None);
    }

    #[tokio::test]
    async fn run_session_new_none_target_forces_empty_daytona_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let mut settings = settings_from_run_layer(RunLayer {
            clone: Some(RunCloneLayer {
                enabled: Some(true),
                depth:   None,
            }),
            ..RunLayer::default()
        });
        settings.run.environment.provider = EnvironmentProvider::Daytona;
        settings.run.environment.image.docker = None;
        let (persisted, store) = persisted_workflow_with_settings_and_target(
            MINIMAL_DOT,
            &storage_root,
            settings,
            Some(RunTarget::None {}),
        )
        .await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());
        let vault = Arc::new(AsyncRwLock::new(start_vault(&[(
            EnvVars::DAYTONA_API_KEY,
            "test-daytona-key",
            SecretType::Token,
        )])));

        let session = RunSession::new(&persisted, StartServices {
            vault,
            ..test_start_services(&store, &storage_root, emitter, registry).await
        })
        .await
        .unwrap();

        let RunSession {
            sandbox,
            sandbox_env,
            pr_origin_url,
            ..
        } = session;
        let runtime = sandbox
            .to_run_sandbox_instance(&MockSandbox::linux(), fixtures::RUN_1)
            .runtime;
        assert_eq!(runtime.repo_cloned, Some(false));
        assert_eq!(runtime.clone_origin_url, None);
        assert_eq!(runtime.clone_branch, None);
        assert_eq!(runtime.primary_repo_path, None);
        assert_eq!(runtime.primary_repo_link, None);
        let SandboxSpec::Daytona {
            config,
            clone_origin_url,
            clone_branch,
            clone_commit_sha,
            ..
        } = sandbox
        else {
            panic!("none target should retain the selected Daytona provider");
        };
        assert!(config.skip_clone);
        assert_eq!(clone_origin_url, None);
        assert_eq!(clone_branch, None);
        assert_eq!(clone_commit_sha, None);
        assert_eq!(sandbox_env.origin_url, None);
        assert_eq!(pr_origin_url, None);
    }

    #[tokio::test]
    async fn run_session_new_rejects_persisted_none_target_with_local_provider() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let mut settings = settings_from_run_layer(RunLayer::default());
        settings.run.environment.provider = EnvironmentProvider::Local;
        let (persisted, store) = persisted_workflow_with_settings_and_target(
            MINIMAL_DOT,
            &storage_root,
            settings,
            Some(RunTarget::None {}),
        )
        .await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let Err(error) = RunSession::new(
            &persisted,
            test_start_services(&store, &storage_root, emitter, registry).await,
        )
        .await
        else {
            panic!("persisted none target with Local should fail before sandbox creation");
        };

        assert!(error.to_string().contains("none run targets require"));
    }

    #[tokio::test]
    async fn run_session_new_dry_run_clone_targets_use_isolated_local_workspace() {
        for target in [
            RunTarget::None {},
            RunTarget::Git(GitRunTarget {
                repo:   "fabro-sh/fabro".to_string(),
                branch: "main".to_string(),
                tag:    None,
                sha:    Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            }),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
            let mut settings = settings_from_run_layer(RunLayer {
                execution: Some(RunExecutionLayer {
                    mode: Some(RunMode::DryRun),
                    ..RunExecutionLayer::default()
                }),
                ..RunLayer::default()
            });
            settings.run.environment.provider = EnvironmentProvider::Docker;
            settings.run.environment.image.docker = Some("buildpack-deps:noble".to_string());
            let (persisted, store) = persisted_workflow_with_settings_and_target(
                MINIMAL_DOT,
                &storage_root,
                settings,
                Some(target.clone()),
            )
            .await;
            assert_eq!(persisted.run_spec().target, Some(target.clone()));
            let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
            let registry = Arc::new(test_registry());

            let session = RunSession::new(
                &persisted,
                test_start_services(&store, &storage_root, emitter, registry).await,
            )
            .await
            .unwrap();

            let SandboxSpec::Local { working_directory } = session.sandbox else {
                panic!("clone target dry-run should execute in a Local scratch sandbox");
            };
            assert_eq!(
                working_directory,
                run_dir.join("dry-run-workspace").canonicalize().unwrap()
            );
            assert_eq!(session.sandbox_env.origin_url, None);
            assert_eq!(session.pr_origin_url, None);
            assert!(session.git.is_none());
            assert_eq!(persisted.run_spec().target, Some(target));
        }
    }

    #[tokio::test]
    async fn run_session_new_dry_run_rejects_configured_target_mismatches() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let mut local_settings = settings_from_run_layer(RunLayer {
            execution: Some(RunExecutionLayer {
                mode: Some(RunMode::DryRun),
                ..RunExecutionLayer::default()
            }),
            ..RunLayer::default()
        });
        local_settings.run.environment.provider = EnvironmentProvider::Local;
        let (persisted, store) = persisted_workflow_with_settings_and_target(
            MINIMAL_DOT,
            &storage_root,
            local_settings,
            Some(RunTarget::None {}),
        )
        .await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());
        let Err(error) = RunSession::new(
            &persisted,
            test_start_services(&store, &storage_root, emitter, registry).await,
        )
        .await
        else {
            panic!("Local configured provider must reject none even in dry-run");
        };
        assert!(error.to_string().contains("none run targets require"));

        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let (_, canonical_text) = canonical_folder(&temp);
        let mut docker_settings = settings_from_run_layer(RunLayer {
            execution: Some(RunExecutionLayer {
                mode: Some(RunMode::DryRun),
                ..RunExecutionLayer::default()
            }),
            ..RunLayer::default()
        });
        docker_settings.run.environment.provider = EnvironmentProvider::Docker;
        let (persisted, store) =
            persisted_workflow_with_settings(MINIMAL_DOT, &storage_root, docker_settings).await;
        let persisted = persisted_with_target_projection(
            persisted,
            RunTarget::Folder {
                path: canonical_text.clone(),
            },
            Some(canonical_text),
        );
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());
        let Err(error) = RunSession::new(
            &persisted,
            test_start_services(&store, &storage_root, emitter, registry).await,
        )
        .await
        else {
            panic!("Docker configured provider must reject folder even in dry-run");
        };
        assert!(error.to_string().contains("folder run targets require"));
    }

    #[tokio::test]
    async fn run_session_new_folder_target_uses_canonical_path_and_preserves_git_identity() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let (canonical_folder, canonical_text) = canonical_folder(&temp);
        let environment_cwd = temp.path().join("environment-cwd");
        std::fs::create_dir_all(&environment_cwd).unwrap();
        let mut settings = settings_from_run_layer(RunLayer::default());
        settings.run.environment.provider = EnvironmentProvider::Local;
        settings.run.environment.cwd = Some(environment_cwd.to_string_lossy().into_owned());
        let (persisted, store) =
            persisted_workflow_with_settings(MINIMAL_DOT, &storage_root, settings).await;
        let persisted = persisted_with_target_projection(
            persisted,
            RunTarget::Folder {
                path: canonical_text.clone(),
            },
            Some(canonical_text),
        );
        let origin_url = "https://github.com/acme/widgets";
        let persisted = persisted_with_git_projection(persisted, GitContext {
            origin_url: origin_url.to_string(),
            branch:     "feature".to_string(),
            sha:        Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            dirty:      fabro_types::DirtyStatus::Clean,
        });
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let session = RunSession::new(
            &persisted,
            test_start_services(&store, &storage_root, emitter, registry).await,
        )
        .await
        .unwrap();

        let SandboxSpec::Local { working_directory } = session.sandbox else {
            panic!("folder target should retain the selected Local provider");
        };
        assert_eq!(working_directory, canonical_folder);
        assert_ne!(working_directory, environment_cwd);
        assert_eq!(session.sandbox_env.origin_url.as_deref(), Some(origin_url));
        assert_eq!(session.pr_origin_url.as_deref(), Some(origin_url));
    }

    #[tokio::test]
    async fn run_session_new_folder_target_rejects_clone_based_providers() {
        for provider in [EnvironmentProvider::Docker, EnvironmentProvider::Daytona] {
            let temp = tempfile::tempdir().unwrap();
            let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
            let (_, canonical_text) = canonical_folder(&temp);
            let mut settings = settings_from_run_layer(RunLayer::default());
            settings.run.environment.provider = provider;
            settings.run.environment.image.docker = match provider {
                EnvironmentProvider::Docker => Some("buildpack-deps:noble".to_string()),
                EnvironmentProvider::Daytona | EnvironmentProvider::Local => None,
            };
            let (persisted, store) =
                persisted_workflow_with_settings(MINIMAL_DOT, &storage_root, settings).await;
            let persisted = persisted_with_target_projection(
                persisted,
                RunTarget::Folder {
                    path: canonical_text.clone(),
                },
                Some(canonical_text),
            );
            let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
            let registry = Arc::new(test_registry());

            let Err(error) = RunSession::new(
                &persisted,
                test_start_services(&store, &storage_root, emitter, registry).await,
            )
            .await
            else {
                panic!("folder target with a clone-based provider should fail closed");
            };

            assert!(
                error
                    .to_string()
                    .contains("folder run targets require the Local sandbox provider")
            );
        }
    }

    #[tokio::test]
    async fn run_session_new_legacy_local_run_still_prefers_environment_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, _run_dir) = storage_root_and_run_dir(&temp);
        let environment_cwd = temp.path().join("environment-cwd");
        std::fs::create_dir_all(&environment_cwd).unwrap();
        let mut settings = settings_from_run_layer(RunLayer::default());
        settings.run.environment.provider = EnvironmentProvider::Local;
        settings.run.environment.cwd = Some(environment_cwd.to_string_lossy().into_owned());
        let (persisted, store) =
            persisted_workflow_with_settings(MINIMAL_DOT, &storage_root, settings).await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let registry = Arc::new(test_registry());

        let session = RunSession::new(
            &persisted,
            test_start_services(&store, &storage_root, emitter, registry).await,
        )
        .await
        .unwrap();

        let SandboxSpec::Local { working_directory } = session.sandbox else {
            panic!("legacy Local run should retain the selected Local provider");
        };
        assert_eq!(working_directory, environment_cwd);
    }

    #[tokio::test]
    async fn folder_target_start_rejects_projection_drift() {
        let temp = tempfile::tempdir().unwrap();
        let (_, canonical_text) = canonical_folder(&temp);
        let mut record = test_folder_run_spec(&canonical_text);

        record.source_directory = None;
        let missing_error = folder_working_directory_from_record(&record, &canonical_text)
            .await
            .expect_err("missing source-directory projection should fail");
        assert!(missing_error.to_string().contains("missing"));

        record.source_directory = Some(temp.path().to_string_lossy().into_owned());
        let drift_error = folder_working_directory_from_record(&record, &canonical_text)
            .await
            .expect_err("mismatched source-directory projection should fail");
        assert!(drift_error.to_string().contains("disagrees"));
    }

    #[tokio::test]
    async fn folder_target_start_rejects_relative_and_noncanonical_paths() {
        let relative = "relative/folder";
        let relative_record = test_folder_run_spec(relative);
        let relative_error = folder_working_directory_from_record(&relative_record, relative)
            .await
            .expect_err("relative persisted target should fail");
        assert!(
            relative_error
                .to_string()
                .contains("persisted folder run target path")
        );

        let temp = tempfile::tempdir().unwrap();
        let (canonical_folder, _) = canonical_folder(&temp);
        let noncanonical = canonical_folder
            .join("..")
            .join(canonical_folder.file_name().unwrap());
        let noncanonical_text = noncanonical.to_str().unwrap();
        let noncanonical_record = test_folder_run_spec(noncanonical_text);

        let error = folder_working_directory_from_record(&noncanonical_record, noncanonical_text)
            .await
            .expect_err("noncanonical persisted target should fail");
        assert!(error.to_string().contains("no longer canonical"));
    }

    #[tokio::test]
    async fn folder_target_start_rejects_disappeared_or_retyped_path() {
        let temp = tempfile::tempdir().unwrap();
        let (canonical_folder, canonical_text) = canonical_folder(&temp);
        let record = test_folder_run_spec(&canonical_text);

        std::fs::remove_dir(&canonical_folder).unwrap();
        let missing_error = folder_working_directory_from_record(&record, &canonical_text)
            .await
            .expect_err("disappeared folder target should fail");
        assert!(
            missing_error
                .to_string()
                .contains("could not be canonicalized")
        );
        assert!(!missing_error.causes().is_empty());

        fs::write(&canonical_folder, "not a directory")
            .await
            .unwrap();
        let file_error = folder_working_directory_from_record(&record, &canonical_text)
            .await
            .expect_err("folder target replaced by a file should fail");
        assert!(file_error.to_string().contains("is not a directory"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn folder_target_start_rejects_redirected_path() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let (canonical_folder, canonical_text) = canonical_folder(&temp);
        let redirected = temp.path().join("redirected-target");
        let record = test_folder_run_spec(&canonical_text);
        std::fs::rename(&canonical_folder, &redirected).unwrap();
        symlink(&redirected, &canonical_folder).unwrap();

        let error = folder_working_directory_from_record(&record, &canonical_text)
            .await
            .expect_err("redirected folder target should fail");
        assert!(error.to_string().contains("no longer canonical"));
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

        let config = resolve_docker_config(&settings.run, |_| None).unwrap();

        assert_eq!(config.image, "ubuntu:24.04");
        assert_eq!(config.cpu_quota, Some(400_000));
        assert_eq!(config.memory_limit, Some(2_000_000_000));
        assert_eq!(config.network_mode.as_deref(), Some("none"));
        assert_eq!(config.env_vars, vec!["NODE_ENV=test"]);
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

    async fn persisted_workflow_with_settings(
        dot: &str,
        storage_root: &Path,
        settings: WorkflowSettings,
    ) -> (Persisted, Arc<Database>) {
        persisted_workflow_with_settings_and_target(dot, storage_root, settings, None).await
    }

    async fn persisted_workflow_with_settings_and_target(
        dot: &str,
        storage_root: &Path,
        settings: WorkflowSettings,
        target: Option<RunTarget>,
    ) -> (Persisted, Arc<Database>) {
        let store = memory_store();
        let created = crate::operations::create(
            &store,
            crate::operations::CreateRunInput {
                workflow: crate::operations::WorkflowInput::DotSource {
                    source:   dot.to_string(),
                    base_dir: None,
                },
                settings,
                vars: std::collections::HashMap::new(),
                cwd: storage_root
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf(),
                workflow_slug: Some("test".to_string()),
                workflow_path: None,
                workflow_bundle: None,
                target,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_1),
                title: None,
                automation: None,
                git: None,
                fork_source_ref: None,
                parent_id: None,
                provenance: test_support::test_run_provenance(),
                configured_providers: test_provider_ids(),
                web_url: None,
            },
            storage_root.to_path_buf(),
            test_catalog(),
        )
        .await
        .unwrap();
        (created.persisted, store)
    }

    fn persisted_with_target_projection(
        persisted: Persisted,
        target: RunTarget,
        source_directory: Option<String>,
    ) -> Persisted {
        let (graph, source, diagnostics, run_dir, mut run_spec) = persisted.into_parts();
        run_spec.target = Some(target);
        run_spec.source_directory = source_directory;
        Persisted::new(graph, source, diagnostics, run_dir, run_spec)
    }

    fn persisted_with_git_projection(persisted: Persisted, git: GitContext) -> Persisted {
        let (graph, source, diagnostics, run_dir, mut run_spec) = persisted.into_parts();
        run_spec.git = Some(git);
        Persisted::new(graph, source, diagnostics, run_dir, run_spec)
    }

    /// Create `folder-target` under `temp` and return its canonical path and
    /// the UTF-8 text a persisted folder target would carry.
    fn canonical_folder(temp: &tempfile::TempDir) -> (PathBuf, String) {
        let folder = temp.path().join("folder-target");
        std::fs::create_dir_all(&folder).unwrap();
        let canonical_folder = folder.canonicalize().unwrap();
        let canonical_text = canonical_folder.to_str().unwrap().to_string();
        (canonical_folder, canonical_text)
    }

    fn test_folder_run_spec(path: &str) -> RunSpec {
        let mut record = test_support::test_run_spec();
        record.target = Some(RunTarget::Folder {
            path: path.to_string(),
        });
        record.source_directory = Some(path.to_string());
        record
    }

    async fn persisted_workflow(dot: &str, storage_root: &Path) -> (Persisted, Arc<Database>) {
        persisted_workflow_with_settings(
            dot,
            storage_root,
            settings_from_run_layer(RunLayer {
                execution: Some(RunExecutionLayer {
                    mode: Some(RunMode::DryRun),
                    ..RunExecutionLayer::default()
                }),
                ..RunLayer::default()
            }),
        )
        .await
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
            github_integration: ResolvedGithubIntegration::default(),
            vault: Arc::new(AsyncRwLock::new(start_vault(&[]))),
            catalog: test_catalog(),
            on_node: None,
            registry_override: Some(registry),
            fabro_run_tools: None,
        }
    }

    fn temp_vault(entries: &[(&str, &str, SecretType)]) -> Vault {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        for (name, value, secret_type) in entries {
            vault.set(name, value, *secret_type, None).unwrap();
        }
        vault
    }

    fn token_vault(name: &str, value: &str) -> Vault {
        temp_vault(&[(name, value, SecretType::Token)])
    }

    fn start_vault(entries: &[(&str, &str, SecretType)]) -> Vault {
        let mut all_entries = vec![("ANTHROPIC_API_KEY", "test-key", SecretType::Token)];
        all_entries.extend_from_slice(entries);
        temp_vault(&all_entries)
    }

    fn vault_secret_lookup(vault: &Vault) -> impl FnMut(&str) -> Option<String> + '_ {
        move |name| vault_token_lookup(vault, name)
    }

    fn prepare_with_step(step: PreparedStep) -> RunPrepareSettings {
        RunPrepareSettings {
            steps:      vec![step],
            timeout_ms: 1_000,
        }
    }

    fn script_step(script: &str, env: HashMap<String, String>) -> PreparedStep {
        PreparedStep {
            run: PreparedStepRun::Script {
                script: script.to_string(),
            },
            env,
        }
    }

    fn command_step(command: &[&str], env: HashMap<String, String>) -> PreparedStep {
        PreparedStep {
            run: PreparedStepRun::Command {
                command: command.iter().map(|value| (*value).to_string()).collect(),
            },
            env,
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
                        graph_visit: None,
                        resumed_from_stage_id: None,
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
    async fn event_persistence_failure_stops_execution_and_fails_run() {
        let temp = tempfile::tempdir().unwrap();
        let (storage_root, run_dir) = storage_root_and_run_dir(&temp);
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let mut registry = test_registry();
        registry.register("blocking", Box::new(BlockingHandler));
        let (_persisted, store) = persisted_workflow(BLOCKING_DOT, &storage_root).await;
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        let canonical_sink = RunEventSink::store(run_store.clone());
        let mut services = test_start_services(&store, &run_dir, emitter, Arc::new(registry)).await;
        let cancel_token = services.cancel_token.clone();
        services.event_sink = RunEventSink::callback(move |event| {
            let canonical_sink = canonical_sink.clone();
            async move {
                if matches!(&event.body, EventBody::StageStarted(_))
                    && event.node_id.as_deref() == Some("block")
                {
                    return Err(anyhow::anyhow!(
                        "request failed with status 413 Payload Too Large"
                    )
                    .context("worker lost canonical run store during append run event"));
                }
                canonical_sink.write_run_event(&event).await
            }
        });

        let result = tokio::time::timeout(Duration::from_secs(2), start(&run_dir, services))
            .await
            .expect("event persistence failure should stop the blocking stage");
        let Err(error) = result else {
            panic!("event persistence failure should fail the run");
        };

        assert!(cancel_token.is_cancelled());
        let rendered = error.display_with_causes();
        assert!(
            rendered.contains("run event persistence failed"),
            "{rendered}"
        );
        assert!(rendered.contains("stage.started"), "{rendered}");
        assert!(rendered.contains("413 Payload Too Large"), "{rendered}");

        let projection = run_store.state().await.unwrap();
        assert!(matches!(projection.status, RunStatus::Failed { .. }));
        let events = run_store.list_events().await.unwrap();
        let run_failed = events
            .iter()
            .find_map(|event| match &event.event.body {
                EventBody::RunFailed(properties) => Some(properties),
                _ => None,
            })
            .expect("persistence failure should emit run.failed");
        assert!(
            run_failed
                .failure
                .detail
                .causes
                .iter()
                .any(|cause| cause.contains("413 Payload Too Large"))
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(&event.event.body, EventBody::RunCompleted(_)))
        );
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
                vars: std::collections::HashMap::new(),
                cwd: temp.path().to_path_buf(),
                workflow_slug: Some("bundle-child".to_string()),
                workflow_path: Some(ManifestPath::from_wire("workflow.fabro").unwrap()),
                workflow_bundle: Some(workflow_bundle),
                target: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_1),
                title: None,
                automation: None,
                git: None,
                fork_source_ref: None,
                parent_id: None,
                provenance: test_support::test_run_provenance(),
                configured_providers: test_provider_ids(),
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
                graph_visit: None,
                resumed_from_stage_id: None,
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
            graph_visit: None,
            resumed_from_stage_id: None,
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

    #[test]
    fn clone_commit_legacy_run_never_activates_an_observed_git_sha() {
        let mut spec = test_support::test_run_spec();
        spec.git = Some(fabro_types::GitContext {
            origin_url: "https://github.com/fabro-sh/fabro".to_string(),
            branch:     "main".to_string(),
            sha:        Some("abcdef0123456789abcdef0123456789abcdef01".to_string()),
            dirty:      fabro_types::DirtyStatus::Clean,
        });

        let source = clone_source_for_run(&spec).unwrap();

        assert_eq!(source.commit_sha, None);
        assert_eq!(source.branch.as_deref(), Some("main"));
    }

    #[test]
    fn none_target_forces_an_empty_clone_source_and_workspace() {
        let mut spec = test_support::test_run_spec();
        spec.target = Some(RunTarget::None {});
        spec.git = Some(fabro_types::GitContext {
            origin_url: "https://github.com/fabro-sh/fabro".to_string(),
            branch:     "main".to_string(),
            sha:        Some("abcdef0123456789abcdef0123456789abcdef01".to_string()),
            dirty:      fabro_types::DirtyStatus::Clean,
        });

        let source = clone_source_for_run(&spec).unwrap();

        assert_eq!(source.origin_url, None);
        assert_eq!(source.branch, None);
        assert_eq!(source.commit_sha, None);
        assert!(source.skip_clone);
    }

    #[test]
    fn clone_commit_persisted_git_target_activates_exact_branch_and_sha() {
        let mut spec = test_support::test_run_spec();
        let submitted_sha = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        let normalized_sha = "abcdef0123456789abcdef0123456789abcdef01";
        spec.target = Some(RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "feature/run-intent".to_string(),
            tag:    Some("v1.2.3".to_string()),
            sha:    Some(submitted_sha.to_string()),
        }));
        spec.git = Some(fabro_types::GitContext {
            origin_url: "https://github.com/fabro-sh/fabro".to_string(),
            branch:     "feature/run-intent".to_string(),
            sha:        Some(submitted_sha.to_string()),
            dirty:      fabro_types::DirtyStatus::Clean,
        });

        let source = clone_source_for_run(&spec).unwrap();

        assert_eq!(
            source.origin_url.as_deref(),
            Some("https://github.com/fabro-sh/fabro")
        );
        assert_eq!(source.branch.as_deref(), Some("feature/run-intent"));
        assert_eq!(source.tag.as_deref(), Some("v1.2.3"));
        assert_eq!(source.commit_sha.as_deref(), Some(normalized_sha));
    }

    #[test]
    fn clone_commit_persisted_git_target_without_sha_keeps_branch_unpinned() {
        let mut spec = test_support::test_run_spec();
        spec.target = Some(RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "feature/run-intent".to_string(),
            tag:    None,
            sha:    None,
        }));
        spec.git = Some(fabro_types::GitContext {
            origin_url: "https://github.com/fabro-sh/fabro".to_string(),
            branch:     "feature/run-intent".to_string(),
            sha:        None,
            dirty:      fabro_types::DirtyStatus::Clean,
        });

        let source = clone_source_for_run(&spec).unwrap();

        assert_eq!(source.branch.as_deref(), Some("feature/run-intent"));
        assert_eq!(source.commit_sha, None);
    }

    #[test]
    fn clone_source_preserves_unpinned_tag_separately_from_working_branch() {
        let mut spec = test_support::test_run_spec();
        spec.target = Some(RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "release-work".to_string(),
            tag:    Some("v1.2.3".to_string()),
            sha:    None,
        }));

        let source = clone_source_for_run(&spec).unwrap();

        assert_eq!(source.branch.as_deref(), Some("release-work"));
        assert_eq!(source.tag.as_deref(), Some("v1.2.3"));
        assert_eq!(source.commit_sha, None);
    }

    #[test]
    fn clone_commit_persisted_git_target_is_authoritative_over_projection() {
        let mut spec = test_support::test_run_spec();
        spec.target = Some(RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "main".to_string(),
            tag:    None,
            sha:    None,
        }));
        // A drifted (or absent) projection never feeds the clone source: the
        // validated target alone does.
        spec.git = Some(fabro_types::GitContext {
            origin_url: "https://github.com/fabro-sh/other".to_string(),
            branch:     "other".to_string(),
            sha:        Some("abcdef0123456789abcdef0123456789abcdef01".to_string()),
            dirty:      fabro_types::DirtyStatus::Clean,
        });

        let source = clone_source_for_run(&spec).unwrap();

        assert_eq!(
            source.origin_url.as_deref(),
            Some("https://github.com/fabro-sh/fabro")
        );
        assert_eq!(source.branch.as_deref(), Some("main"));
        assert_eq!(source.commit_sha, None);

        spec.git = None;
        let source = clone_source_for_run(&spec).unwrap();
        assert_eq!(source.branch.as_deref(), Some("main"));
    }
}
