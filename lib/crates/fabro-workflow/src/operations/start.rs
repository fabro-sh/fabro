use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use fabro_config::FabroSettings;
use fabro_config::sandbox::WorktreeMode;
use fabro_config::{project as project_config, run as run_config, sandbox as sandbox_config};
use fabro_interview::{AutoApproveInterviewer, Interviewer};
use fabro_model::{Catalog, FallbackTarget, Provider};
use fabro_sandbox::{SandboxProvider, SandboxSpec};
use fabro_store::{DiskProjectingRunStore, ProjectionError, RunStore};
use fabro_types::RunId;
use serde::Serialize;

use crate::context::Context;
use crate::error::FabroError;
use crate::event::{
    EventEmitter, RunNoticeLevel, StoreProgressLogger, WorkflowRunEvent, append_progress_event,
    build_redacted_event_payload,
};
use crate::handler::HandlerRegistry;
use crate::outcome::{Outcome, StageStatus};
use crate::pipeline::{
    self, DevcontainerSpec, FinalizeOptions, Finalized, InitOptions, LlmSpec, Persisted,
    PullRequestOptions, RetroOptions, SandboxEnvSpec, build_conclusion_from_store,
    classify_engine_result,
};
use crate::records::{Checkpoint, Conclusion, ConclusionExt, RunRecord, RunRecordExt};
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions};
use crate::run_status::{self, RunStatus, StatusReason};
use fabro_config::run::PullRequestSettings;
use fabro_retro::retro::Retro;
use fabro_sandbox::daytona::DaytonaConfig;
use fabro_sandbox::daytona::detect_repo_info;
use tokio::runtime::Handle;

struct RunSession {
    cancel_token: Option<Arc<AtomicBool>>,
    emitter: Arc<EventEmitter>,
    sandbox: SandboxSpec,
    llm: LlmSpec,
    interviewer: Arc<dyn Interviewer>,
    on_node: crate::OnNodeCallback,
    lifecycle: LifecycleOptions,
    hooks: fabro_hooks::HookConfig,
    sandbox_env: SandboxEnvSpec,
    devcontainer: Option<DevcontainerSpec>,
    seed_context: Option<Context>,
    run_store: Arc<dyn RunStore>,
    git: Option<GitCheckpointOptions>,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    worktree_mode: Option<WorktreeMode>,
    registry_override: Option<Arc<HandlerRegistry>>,
    retro_enabled: bool,
    preserve_sandbox: bool,
    pr_config: Option<PullRequestSettings>,
    pr_github_app: Option<fabro_github::GitHubAppCredentials>,
    pr_origin_url: Option<String>,
    pr_model: String,
}

pub struct StartServices {
    pub cancel_token: Option<Arc<AtomicBool>>,
    pub emitter: Arc<EventEmitter>,
    pub interviewer: Arc<dyn Interviewer>,
    pub run_store: Arc<dyn RunStore>,
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    pub on_node: crate::OnNodeCallback,
    pub registry_override: Option<Arc<HandlerRegistry>>,
}

pub struct Started {
    pub finalized: Finalized,
    pub final_context: Option<Context>,
    pub retro: Option<Retro>,
    pub retro_duration: Duration,
}

/// Start a fresh workflow run. Errors if a checkpoint already exists (use `resume()` instead).
pub async fn start(run_dir: &Path, services: StartServices) -> Result<Started, FabroError> {
    if services
        .run_store
        .get_checkpoint()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?
        .is_some()
    {
        return Err(FabroError::Precondition(
            "checkpoint.json exists in run directory — did you mean to resume?".to_string(),
        ));
    }

    if let Some(record) = services
        .run_store
        .get_status()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?
    {
        if !matches!(record.status, RunStatus::Submitted | RunStatus::Starting) {
            return Err(FabroError::Precondition(format!(
                "cannot start run: status is {:?}, expected submitted",
                record.status
            )));
        }
    }

    Box::pin(execute_persisted_run(run_dir, None, services)).await
}

pub(super) async fn execute_persisted_run(
    run_dir: &Path,
    checkpoint: Option<Checkpoint>,
    mut services: StartServices,
) -> Result<Started, FabroError> {
    let cancel_token = services.cancel_token.clone();
    let inner_store = Arc::clone(&services.run_store);
    let projection_run_dir = run_dir.to_path_buf();
    services.run_store = Arc::new(
        DiskProjectingRunStore::new(inner_store, run_dir.to_path_buf()).on_projection_error(
            Arc::new(move |projection_error: ProjectionError| {
                let Some(run_id) = load_run_id(&projection_run_dir) else {
                    return;
                };

                // Write directly to progress.jsonl/live.json so projection failures do not
                // recurse back through the decorated store.append_event() path.
                let _ = append_progress_event(
                    &projection_run_dir,
                    &run_id,
                    &WorkflowRunEvent::RunNotice {
                        level: RunNoticeLevel::Warn,
                        code: "disk_projection_failed".to_string(),
                        message: format!(
                            "{}disk projection failed for {}: {}",
                            if projection_error.critical {
                                "critical "
                            } else {
                                ""
                            },
                            projection_error.path.display(),
                            projection_error.error
                        ),
                    },
                );
            }),
        ),
    );
    let run_store = Arc::clone(&services.run_store);
    if let Err(err) = run_store
        .put_status(&run_status::RunStatusRecord::new(
            RunStatus::Starting,
            Some(StatusReason::SandboxInitializing),
        ))
        .await
    {
        let error = FabroError::engine(err.to_string());
        let _ = persist_detached_failure(
            run_store.as_ref(),
            run_dir,
            "bootstrap",
            StatusReason::BootstrapFailed,
            &error,
        )
        .await;
        return Err(error);
    }

    let mut bootstrap_guard =
        DetachedRunBootstrapGuard::arm(run_dir, Arc::clone(&run_store), cancel_token.clone());

    let persisted = match Persisted::load_from_store(services.run_store.as_ref(), run_dir).await {
        Ok(persisted) => persisted,
        Err(err) => {
            let _ = persist_detached_failure(
                run_store.as_ref(),
                run_dir,
                "bootstrap",
                StatusReason::BootstrapFailed,
                &err,
            )
            .await;
            bootstrap_guard.defuse();
            return Err(err);
        }
    };

    let session = match RunSession::new(&persisted, services) {
        Ok(session) => session,
        Err(err) => {
            let _ = persist_detached_failure(
                run_store.as_ref(),
                run_dir,
                "bootstrap",
                StatusReason::BootstrapFailed,
                &err,
            )
            .await;
            bootstrap_guard.defuse();
            return Err(err);
        }
    };

    bootstrap_guard.defuse();
    let mut completion_guard =
        DetachedRunCompletionGuard::arm(run_dir, Arc::clone(&run_store), cancel_token);
    let run_start = Instant::now();
    let started = Box::pin(session.run(persisted, checkpoint)).await;

    match started {
        Ok(started) => {
            completion_guard.defuse();
            Ok(started)
        }
        Err(err) => {
            persist_terminal_engine_failure(run_store.as_ref(), run_dir, &err, run_start.elapsed())
                .await;
            completion_guard.defuse();
            Err(err)
        }
    }
}

async fn persist_terminal_engine_failure(
    run_store: &dyn RunStore,
    run_dir: &Path,
    error: &FabroError,
    duration: Duration,
) {
    let engine_result: Result<Outcome, FabroError> = Err(error.clone());
    let (final_status, failure_reason, run_status, status_reason) =
        classify_engine_result(&engine_result);
    let conclusion = build_conclusion_from_store(
        run_store,
        run_dir,
        final_status,
        failure_reason,
        u64::try_from(duration.as_millis()).unwrap(),
        None,
    )
    .await;
    if let Err(err) = run_store.put_conclusion(&conclusion).await {
        tracing::warn!(error = %err, "Failed to save terminal engine failure conclusion to store");
    }
    if let Err(err) = run_store
        .put_status(&run_status::RunStatusRecord::new(run_status, status_reason))
        .await
    {
        tracing::warn!(error = %err, "Failed to save terminal engine failure status to store");
    }
}

impl RunSession {
    fn new(persisted: &Persisted, services: StartServices) -> Result<Self, FabroError> {
        let record = persisted.run_record();
        let mut settings = record.settings.clone();
        let working_directory = record.working_directory.clone();

        if let Some(env) = settings
            .sandbox
            .as_mut()
            .and_then(|sandbox| sandbox.env.as_mut())
        {
            run_config::resolve_env_refs(env)
                .map_err(|err| FabroError::Precondition(err.to_string()))?;
        }

        let (origin_url, detected_base_branch) = detect_repo_info(&working_directory)
            .map(|(url, branch)| (Some(url), branch))
            .unwrap_or((None, None));

        let sandbox_provider = resolve_sandbox_provider(&settings)?;
        let sandbox_provider = if settings.dry_run_enabled() && !sandbox_provider.is_local() {
            SandboxProvider::Local
        } else {
            sandbox_provider
        };
        let model = settings
            .llm
            .as_ref()
            .and_then(|llm| llm.model.clone())
            .unwrap_or_else(|| Catalog::builtin().default_from_env().id.clone());
        let provider = settings
            .llm
            .as_ref()
            .and_then(|llm| llm.provider.clone())
            .filter(|value| !value.is_empty());

        let provider_enum: Provider = provider
            .as_deref()
            .map(str::parse::<Provider>)
            .transpose()
            .map_err(|err| FabroError::Precondition(err.clone()))?
            .unwrap_or_else(Provider::default_from_env);

        let fallback_chain = resolve_fallback_chain(provider_enum, &model, &settings);
        let mcp_servers = settings
            .mcp_server_entries()
            .clone()
            .into_iter()
            .map(|(name, entry)| entry.into_config(name))
            .collect();

        let sandbox = match sandbox_provider {
            SandboxProvider::Local => SandboxSpec::Local {
                working_directory: working_directory.clone(),
            },
            SandboxProvider::Docker => SandboxSpec::Docker {
                config: fabro_agent::DockerSandboxConfig {
                    host_working_directory: working_directory.to_string_lossy().to_string(),
                    ..Default::default()
                },
            },
            SandboxProvider::Daytona => SandboxSpec::Daytona {
                config: resolve_daytona_config(&settings).unwrap_or_default(),
                github_app: services.github_app.clone(),
                run_id: Some(record.run_id),
                clone_branch: detected_base_branch.or_else(|| record.base_branch.clone()),
            },
        };

        let sandbox_env = SandboxEnvSpec {
            devcontainer_env: HashMap::new(),
            toml_env: settings
                .sandbox_settings()
                .and_then(|sandbox| sandbox.env.clone())
                .unwrap_or_default(),
            github_permissions: settings.github_permissions().cloned(),
            origin_url: origin_url.clone(),
        };

        let devcontainer = settings
            .sandbox_settings()
            .and_then(|sandbox| sandbox.devcontainer)
            .unwrap_or(false)
            .then(|| DevcontainerSpec {
                enabled: true,
                resolve_dir: working_directory.clone(),
            });

        let interviewer: Arc<dyn Interviewer> = if settings.auto_approve_enabled() {
            Arc::new(AutoApproveInterviewer)
        } else {
            services.interviewer
        };

        Ok(Self {
            cancel_token: services.cancel_token,
            emitter: services.emitter,
            sandbox,
            llm: LlmSpec {
                model: model.clone(),
                provider: provider_enum,
                fallback_chain,
                mcp_servers,
                dry_run: settings.dry_run_enabled(),
            },
            interviewer,
            on_node: services.on_node,
            lifecycle: LifecycleOptions {
                setup_commands: settings.setup_commands().to_vec(),
                setup_command_timeout_ms: settings.setup_timeout_ms().unwrap_or(300_000),
                devcontainer_phases: Vec::new(),
            },
            hooks: fabro_hooks::HookConfig {
                hooks: settings.hooks.clone(),
            },
            sandbox_env,
            devcontainer,
            seed_context: None,
            run_store: services.run_store,
            git: None,
            github_app: services.github_app.clone(),
            worktree_mode: Some(resolve_worktree_mode(&settings)),
            registry_override: services.registry_override,
            retro_enabled: !settings.no_retro_enabled() && project_config::is_retro_enabled(),
            preserve_sandbox: resolve_preserve_sandbox(&settings),
            pr_config: settings.pull_request.clone(),
            pr_github_app: services.github_app,
            pr_origin_url: origin_url,
            pr_model: model,
        })
    }
}

fn resolve_sandbox_provider(settings: &FabroSettings) -> Result<SandboxProvider, FabroError> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.provider.as_deref())
        .map(str::parse::<SandboxProvider>)
        .transpose()
        .map_err(|err| FabroError::Precondition(format!("Invalid sandbox provider: {err}")))?
        .map_or_else(|| Ok(SandboxProvider::default()), Ok)
}

fn resolve_preserve_sandbox(settings: &FabroSettings) -> bool {
    settings.preserve_sandbox_enabled()
}

fn resolve_worktree_mode(settings: &FabroSettings) -> sandbox_config::WorktreeMode {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.local.as_ref())
        .map(|local| local.worktree_mode)
        .unwrap_or_default()
}

fn resolve_daytona_config(settings: &FabroSettings) -> Option<DaytonaConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.daytona.clone())
}

fn resolve_fallback_chain(
    provider: Provider,
    model: &str,
    settings: &FabroSettings,
) -> Vec<FallbackTarget> {
    let fallbacks = settings.llm.as_ref().and_then(|llm| llm.fallbacks.as_ref());

    match fallbacks {
        Some(map) => Catalog::builtin().build_fallback_chain(provider, model, map),
        None => Vec::new(),
    }
}

impl RunSession {
    /// Shared engine: initialize, execute, retro, finalize, pull_request.
    async fn run(
        self,
        persisted: Persisted,
        checkpoint: Option<Checkpoint>,
    ) -> Result<Started, FabroError> {
        let preserve_sandbox = self.preserve_sandbox;
        let on_node = self.on_node.clone();

        let record = persisted.run_record();
        let run_options = RunOptions {
            settings: record.settings.clone(),
            run_dir: persisted.run_dir().to_path_buf(),
            cancel_token: self.cancel_token,
            run_id: record.run_id,
            labels: record.labels.clone(),
            workflow_slug: record.workflow_slug.clone(),
            github_app: self.github_app.clone(),
            host_repo_path: record.host_repo_path.as_deref().map(PathBuf::from),
            base_branch: record.base_branch.clone(),
            display_base_sha: None,
            git: self.git.clone(),
        };

        let last_git_sha: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let sha_clone = Arc::clone(&last_git_sha);
            self.emitter.on_event(move |event| match event {
                WorkflowRunEvent::CheckpointCompleted {
                    git_commit_sha: Some(sha),
                    ..
                }
                | WorkflowRunEvent::WorkflowRunCompleted {
                    final_git_commit_sha: Some(sha),
                    ..
                }
                | WorkflowRunEvent::GitCommit { sha, .. } => {
                    *sha_clone.lock().unwrap() = Some(sha.clone());
                }
                _ => {}
            });
        }

        let store_progress_logger =
            StoreProgressLogger::new(Arc::clone(&self.run_store), record.run_id);
        store_progress_logger.register(self.emitter.as_ref());

        let init_options = InitOptions {
            run_id: record.run_id,
            run_store: Arc::clone(&self.run_store),
            dry_run: run_options.dry_run_enabled(),
            emitter: self.emitter,
            sandbox: self.sandbox,
            llm: self.llm,
            interviewer: self.interviewer,
            lifecycle: self.lifecycle,
            run_options,
            hooks: self.hooks,
            sandbox_env: self.sandbox_env,
            devcontainer: self.devcontainer,
            git: self.git,
            worktree_mode: self.worktree_mode,
            registry_override: self.registry_override,
            checkpoint,
            seed_context: self.seed_context,
        };
        let mut initialized = pipeline::initialize(persisted, init_options).await?;
        initialized.on_node = on_node;

        let sandbox_for_cleanup = Arc::clone(&initialized.sandbox);
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
            run_store: Arc::clone(&executed.run_store),
            workflow_name: executed.graph.name.clone(),
            goal: executed.graph.goal().to_string(),
            run_dir: executed.run_options.run_dir.clone(),
            sandbox: Arc::clone(&executed.sandbox),
            emitter: Some(Arc::clone(&executed.emitter)),
            failed,
            run_duration_ms: executed.duration_ms,
            enabled: self.retro_enabled,
            llm_client: executed.llm_client.clone(),
            provider: executed.provider,
            model: executed.model.clone(),
        };

        let retro_start = Instant::now();
        let retroed = pipeline::retro(executed, &retro_opts).await;
        let retro_duration = retro_start.elapsed();

        let finalize_opts = FinalizeOptions {
            run_dir: retroed.run_options.run_dir.clone(),
            run_id: retroed.run_options.run_id,
            run_store: Arc::clone(&retroed.run_store),
            workflow_name: retroed.graph.name.clone(),
            hook_runner: retroed.hook_runner.clone(),
            preserve_sandbox: self.preserve_sandbox,
            last_git_sha: last_git_sha.lock().unwrap().clone(),
        };
        let pr_opts = PullRequestOptions {
            run_dir: retroed.run_options.run_dir.clone(),
            run_store: Some(Arc::clone(&retroed.run_store)),
            pr_config: self.pr_config,
            github_app: self.pr_github_app,
            origin_url: self.pr_origin_url,
            model: self.pr_model,
        };

        let retro = retroed.retro.clone();
        let concluded = pipeline::finalize(retroed, &finalize_opts).await?;
        let finalized = pipeline::pull_request(concluded, &pr_opts).await;
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
    run_dir: PathBuf,
    run_store: Arc<dyn RunStore>,
    cancel_token: Option<Arc<AtomicBool>>,
    active: bool,
}

impl DetachedRunBootstrapGuard {
    fn arm(
        run_dir: &Path,
        run_store: Arc<dyn RunStore>,
        cancel_token: Option<Arc<AtomicBool>>,
    ) -> Self {
        run_status::write_run_status(
            run_dir,
            RunStatus::Starting,
            Some(StatusReason::SandboxInitializing),
        );
        Self {
            run_dir: run_dir.to_path_buf(),
            run_store,
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
                StatusReason::Cancelled
            } else {
                StatusReason::SandboxInitFailed
            };
            run_status::write_run_status(&self.run_dir, RunStatus::Failed, Some(reason));
            let run_store = Arc::clone(&self.run_store);
            if let Ok(handle) = Handle::try_current() {
                handle.spawn(async move {
                    let _ = run_store
                        .put_status(&run_status::RunStatusRecord::new(
                            RunStatus::Failed,
                            Some(reason),
                        ))
                        .await;
                });
            }
        }
    }
}

const POSTRUN_ABORTED_MESSAGE: &str = "Run aborted before post-run finalization completed.";
const POSTRUN_CANCELLED_MESSAGE: &str = "Run cancelled before post-run finalization completed.";

struct DetachedRunCompletionGuard {
    run_dir: PathBuf,
    run_store: Arc<dyn RunStore>,
    run_id: Option<RunId>,
    cancel_token: Option<Arc<AtomicBool>>,
    active: bool,
}

impl DetachedRunCompletionGuard {
    fn arm(
        run_dir: &Path,
        run_store: Arc<dyn RunStore>,
        cancel_token: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self {
            run_dir: run_dir.to_path_buf(),
            run_store,
            run_id: load_run_id(run_dir),
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
            StatusReason::Cancelled
        } else {
            StatusReason::WorkflowError
        };
        let message = if cancelled {
            POSTRUN_CANCELLED_MESSAGE
        } else {
            POSTRUN_ABORTED_MESSAGE
        };
        let code = if cancelled {
            "postrun_cancelled"
        } else {
            "postrun_aborted"
        };

        run_status::write_run_status(&self.run_dir, RunStatus::Failed, Some(reason));
        if !self.run_dir.join("conclusion.json").exists() {
            let _ = write_failure_conclusion(&self.run_dir, message, Some(reason));
        }
        if let Some(run_id) = load_run_id(&self.run_dir) {
            let _ = append_progress_event(
                &self.run_dir,
                &run_id,
                &WorkflowRunEvent::RunNotice {
                    level: RunNoticeLevel::Error,
                    code: code.to_string(),
                    message: message.to_string(),
                },
            );
        }
        let run_store = Arc::clone(&self.run_store);
        let run_id = self.run_id;
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                let _ = run_store
                    .put_status(&run_status::RunStatusRecord::new(
                        RunStatus::Failed,
                        Some(reason),
                    ))
                    .await;
                if let Err(err) = run_store
                    .put_conclusion(&build_failure_conclusion(message))
                    .await
                {
                    tracing::warn!(
                        error = %err,
                        "Failed to save post-run abort conclusion to store"
                    );
                }
                if let Some(run_id) = run_id {
                    let event = WorkflowRunEvent::RunNotice {
                        level: RunNoticeLevel::Error,
                        code: code.to_string(),
                        message: message.to_string(),
                    };
                    match build_redacted_event_payload(&event, &run_id) {
                        Ok(payload) => {
                            let _ = run_store.append_event(&payload).await;
                        }
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "Failed to build post-run abort event payload"
                            );
                        }
                    }
                }
            });
        }
    }
}

fn load_run_id(run_dir: &Path) -> Option<RunId> {
    RunRecord::load(run_dir)
        .ok()
        .map(|record| record.run_id)
        .or_else(|| {
            std::fs::read_to_string(run_dir.join("id.txt"))
                .ok()
                .map(|run_id| run_id.trim().to_string())
                .filter(|run_id| !run_id.is_empty())
                .and_then(|run_id| run_id.parse().ok())
        })
}

async fn persist_detached_failure(
    run_store: &dyn RunStore,
    run_dir: &Path,
    phase: &'static str,
    reason: StatusReason,
    error: &FabroError,
) -> Result<(), FabroError> {
    #[derive(Serialize)]
    struct DetachedFailureRecord<'a> {
        timestamp: chrono::DateTime<Utc>,
        phase: &'a str,
        reason: StatusReason,
        error: String,
    }

    let message = error.to_string();
    let record = DetachedFailureRecord {
        timestamp: Utc::now(),
        phase,
        reason,
        error: message.clone(),
    };

    std::fs::write(
        run_dir.join("detached_failure.json"),
        serde_json::to_string_pretty(&record).map_err(|err| FabroError::Io(err.to_string()))?,
    )
    .map_err(|err| FabroError::Io(err.to_string()))?;

    let conclusion = write_failure_conclusion(run_dir, &message, Some(reason))?;
    run_status::write_run_status(run_dir, RunStatus::Failed, Some(reason));
    if let Err(err) = run_store.put_conclusion(&conclusion).await {
        tracing::warn!(error = %err, "Failed to save detached failure conclusion to store");
    }
    if let Err(err) = run_store
        .put_status(&run_status::RunStatusRecord::new(
            RunStatus::Failed,
            Some(reason),
        ))
        .await
    {
        tracing::warn!(error = %err, "Failed to save detached failure status to store");
    }

    if let Some(run_id) = load_run_id(run_dir) {
        append_progress_event(
            run_dir,
            &run_id,
            &WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Error,
                code: format!("{phase}_failed"),
                message: message.clone(),
            },
        )
        .map_err(|err| FabroError::Io(err.to_string()))?;
        let event = WorkflowRunEvent::RunNotice {
            level: RunNoticeLevel::Error,
            code: format!("{phase}_failed"),
            message,
        };
        match build_redacted_event_payload(&event, &run_id) {
            Ok(payload) => {
                if let Err(err) = run_store.append_event(&payload).await {
                    tracing::warn!(error = %err, "Failed to append detached failure event to store");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "Failed to build detached failure event payload");
            }
        }
    }

    Ok(())
}

fn write_failure_conclusion(
    run_dir: &Path,
    message: &str,
    _reason: Option<StatusReason>,
) -> Result<Conclusion, FabroError> {
    if run_dir.join("conclusion.json").exists() {
        return Conclusion::load(&run_dir.join("conclusion.json"));
    }

    let conclusion = build_failure_conclusion(message);
    conclusion.save(&run_dir.join("conclusion.json"))?;
    Ok(conclusion)
}

fn build_failure_conclusion(message: &str) -> Conclusion {
    Conclusion {
        timestamp: Utc::now(),
        status: StageStatus::Fail,
        duration_ms: 0,
        failure_reason: Some(message.to_string()),
        final_git_commit_sha: None,
        stages: vec![],
        total_cost: None,
        total_retries: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        total_cache_write_tokens: 0,
        total_reasoning_tokens: 0,
        has_pricing: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    use chrono::Utc;
    use fabro_config::FabroSettings;
    use fabro_store::InMemoryStore;
    use fabro_types::fixtures;

    use super::*;
    use crate::context::Context;
    use crate::event::EventEmitter;
    use crate::handler::HandlerRegistry;
    use crate::handler::exit::ExitHandler;
    use crate::handler::start::StartHandler;
    use crate::operations::resume;
    use crate::records::CheckpointExt;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    fn persisted_workflow(dot: &str, run_dir: &Path) -> Persisted {
        crate::operations::create(crate::operations::CreateRunInput {
            workflow: crate::operations::WorkflowInput::DotSource {
                source: dot.to_string(),
                base_dir: None,
            },
            settings: FabroSettings {
                dry_run: Some(true),
                ..Default::default()
            },
            cwd: run_dir
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            workflow_slug: Some("test".to_string()),
            run_dir: Some(run_dir.to_path_buf()),
            run_id: Some(fixtures::RUN_1),
            host_repo_path: None,
            base_branch: None,
        })
        .unwrap()
        .persisted
    }

    fn test_registry() -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry
    }

    async fn test_start_services(
        run_dir: &Path,
        emitter: Arc<EventEmitter>,
        registry: Arc<HandlerRegistry>,
    ) -> StartServices {
        StartServices {
            cancel_token: None,
            emitter,
            interviewer: Arc::new(fabro_interview::AutoApproveInterviewer),
            run_store: crate::operations::open_or_hydrate_run(&InMemoryStore::default(), run_dir)
                .await
                .unwrap(),
            github_app: None,
            on_node: None,
            registry_override: Some(registry),
        }
    }

    #[tokio::test]
    async fn start_captures_checkpoint_git_sha_in_conclusion() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());
        let injected = Arc::new(AtomicBool::new(false));

        {
            let injected = Arc::clone(&injected);
            let emitter_for_injection = Arc::clone(&emitter);
            emitter.on_event(move |event| {
                if injected.load(Ordering::SeqCst) {
                    return;
                }
                if let WorkflowRunEvent::StageStarted { node_id, .. } = event {
                    if node_id == "start" {
                        injected.store(true, Ordering::SeqCst);
                        emitter_for_injection.emit(&WorkflowRunEvent::CheckpointCompleted {
                            node_id: node_id.clone(),
                            status: "success".to_string(),
                            git_commit_sha: Some("sha-test".to_string()),
                        });
                    }
                }
            });
        }

        persisted_workflow(MINIMAL_DOT, &run_dir);
        let started = start(
            &run_dir,
            test_start_services(&run_dir, emitter, registry).await,
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
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());

        persisted_workflow(MINIMAL_DOT, &run_dir);

        let started = start(
            &run_dir,
            test_start_services(&run_dir, emitter, registry).await,
        )
        .await
        .unwrap();

        assert_eq!(started.finalized.conclusion.status, StageStatus::Success);
        assert!(run_dir.join("conclusion.json").exists());
    }

    #[tokio::test]
    async fn start_invokes_on_node_callback_before_execution() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());
        let visited = Arc::new(Mutex::new(Vec::new()));

        persisted_workflow(MINIMAL_DOT, &run_dir);

        let started = start(
            &run_dir,
            StartServices {
                on_node: Some(Arc::new({
                    let visited = Arc::clone(&visited);
                    move |node_id: &str| {
                        visited.lock().unwrap().push(node_id.to_string());
                    }
                })),
                ..test_start_services(&run_dir, emitter, registry).await
            },
        )
        .await
        .unwrap();

        assert_eq!(started.finalized.conclusion.status, StageStatus::Success);
        assert_eq!(*visited.lock().unwrap(), vec!["start".to_string()]);
    }

    #[tokio::test]
    async fn start_errors_when_checkpoint_exists() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());

        persisted_workflow(MINIMAL_DOT, &run_dir);
        let services = test_start_services(&run_dir, emitter, registry).await;

        // Write a checkpoint to the store (not disk) so start() sees it
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
        services
            .run_store
            .put_checkpoint(&checkpoint)
            .await
            .unwrap();

        let result = start(&run_dir, services).await;

        assert!(
            matches!(&result, Err(crate::error::FabroError::Precondition(_))),
            "expected Precondition error, got: {result:?}",
            result = result.as_ref().map(|_| "Ok"),
        );
    }

    #[tokio::test]
    async fn resume_errors_when_checkpoint_missing() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());

        persisted_workflow(MINIMAL_DOT, &run_dir);

        let result = resume(
            &run_dir,
            test_start_services(&run_dir, emitter, registry).await,
        )
        .await;

        assert!(
            matches!(&result, Err(crate::error::FabroError::Precondition(_))),
            "expected Precondition error, got: {result:?}",
            result = result.as_ref().map(|_| "Ok"),
        );
    }

    #[tokio::test]
    async fn resume_errors_when_run_already_finished_successfully() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());

        persisted_workflow(MINIMAL_DOT, &run_dir);

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
        checkpoint.save(&run_dir.join("checkpoint.json")).unwrap();

        crate::records::Conclusion {
            timestamp: Utc::now(),
            status: StageStatus::Success,
            duration_ms: 1,
            failure_reason: None,
            final_git_commit_sha: None,
            stages: vec![],
            total_cost: None,
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: false,
        }
        .save(&run_dir.join("conclusion.json"))
        .unwrap();

        let result = resume(
            &run_dir,
            test_start_services(&run_dir, emitter, registry).await,
        )
        .await;

        assert!(
            matches!(&result, Err(crate::error::FabroError::Precondition(_))),
            "expected Precondition error, got: {result:?}",
            result = result.as_ref().map(|_| "Ok"),
        );
    }
}
