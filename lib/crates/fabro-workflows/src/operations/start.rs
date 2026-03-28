use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use fabro_config::sandbox::WorktreeMode;
use fabro_config::FabroSettings;
use fabro_config::{project as project_config, run as run_config, sandbox as sandbox_config};
use fabro_interview::{AutoApproveInterviewer, Interviewer};
use fabro_model::{Catalog, FallbackTarget, Provider};
use fabro_sandbox::SandboxProvider;
use serde::Serialize;

use crate::context::Context;
use crate::error::FabroError;
use crate::event::{EventEmitter, ProgressLogger, RunNoticeLevel, WorkflowRunEvent};
use crate::outcome::{Outcome, StageStatus};
use crate::pipeline::{
    self, build_conclusion, classify_engine_result, persist_terminal_outcome, DevcontainerSpec,
    FinalizeOptions, Finalized, InitOptions, LlmSpec, Persisted, PullRequestOptions, RetroOptions,
    SandboxEnvSpec, SandboxSpec,
};
use crate::records::{Checkpoint, Conclusion, ConclusionExt, RunRecordExt};
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions};
use crate::run_status::{self, RunStatus, RunStatusRecordExt, StatusReason};

struct RunSession {
    cancel_token: Option<Arc<AtomicBool>>,
    emitter: Arc<EventEmitter>,
    sandbox: SandboxSpec,
    llm: LlmSpec,
    interviewer: Arc<dyn Interviewer>,
    lifecycle: LifecycleOptions,
    hooks: fabro_hooks::HookConfig,
    sandbox_env: SandboxEnvSpec,
    devcontainer: Option<DevcontainerSpec>,
    seed_context: Option<Context>,
    git_author: crate::git::GitAuthor,
    git: Option<GitCheckpointOptions>,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    worktree_mode: Option<WorktreeMode>,
    registry_override: Option<Arc<crate::handler::HandlerRegistry>>,
    retro_enabled: bool,
    preserve_sandbox: bool,
    pr_config: Option<fabro_config::run::PullRequestSettings>,
    pr_github_app: Option<fabro_github::GitHubAppCredentials>,
    pr_origin_url: Option<String>,
    pr_model: String,
}

pub struct StartServices {
    pub cancel_token: Option<Arc<AtomicBool>>,
    pub emitter: Arc<EventEmitter>,
    pub interviewer: Arc<dyn Interviewer>,
    pub git_author: crate::git::GitAuthor,
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    pub registry_override: Option<Arc<crate::handler::HandlerRegistry>>,
}

pub struct Started {
    pub finalized: Finalized,
    pub retro: Option<fabro_retro::retro::Retro>,
    pub retro_duration: Duration,
}

/// Start a fresh workflow run. Errors if a checkpoint already exists (use `resume()` instead).
pub async fn start(run_dir: &Path, services: StartServices) -> Result<Started, FabroError> {
    if run_dir.join("checkpoint.json").exists() {
        return Err(FabroError::Precondition(
            "checkpoint.json exists in run directory — did you mean to resume?".to_string(),
        ));
    }

    if let Ok(record) = run_status::RunStatusRecord::load(&run_dir.join("status.json")) {
        if !matches!(record.status, RunStatus::Submitted | RunStatus::Starting) {
            return Err(FabroError::Precondition(format!(
                "cannot start run: status is {:?}, expected submitted",
                record.status
            )));
        }
    }

    execute_persisted_run(run_dir, None, services).await
}

pub(super) async fn execute_persisted_run(
    run_dir: &Path,
    checkpoint: Option<Checkpoint>,
    services: StartServices,
) -> Result<Started, FabroError> {
    let mut bootstrap_guard = DetachedRunBootstrapGuard::arm(run_dir)?;

    let persisted = match Persisted::load(run_dir) {
        Ok(persisted) => persisted,
        Err(err) => {
            let _ =
                persist_detached_failure(run_dir, "bootstrap", StatusReason::BootstrapFailed, &err);
            bootstrap_guard.defuse();
            return Err(err);
        }
    };

    let session = match RunSession::new(&persisted, services) {
        Ok(session) => session,
        Err(err) => {
            let _ =
                persist_detached_failure(run_dir, "bootstrap", StatusReason::BootstrapFailed, &err);
            bootstrap_guard.defuse();
            return Err(err);
        }
    };

    bootstrap_guard.defuse();
    let mut completion_guard = DetachedRunCompletionGuard::arm(run_dir);
    let run_start = Instant::now();
    let started = session.run(persisted, checkpoint).await;

    match started {
        Ok(started) => {
            completion_guard.defuse();
            Ok(started)
        }
        Err(err) => {
            persist_terminal_engine_failure(run_dir, &err, run_start.elapsed());
            completion_guard.defuse();
            Err(err)
        }
    }
}

fn persist_terminal_engine_failure(run_dir: &Path, error: &FabroError, duration: Duration) {
    let engine_result: Result<Outcome, FabroError> = Err(error.clone());
    let (final_status, failure_reason, run_status, status_reason) =
        classify_engine_result(&engine_result);
    let conclusion = build_conclusion(
        run_dir,
        final_status,
        failure_reason,
        duration.as_millis() as u64,
        None,
    );
    persist_terminal_outcome(run_dir, &conclusion, run_status, status_reason);
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

        let (origin_url, detected_base_branch) =
            fabro_sandbox::daytona::detect_repo_info(&working_directory)
                .map(|(url, branch)| (Some(url), branch))
                .unwrap_or((None, None));

        let sandbox_provider = resolve_sandbox_provider(&settings)?;
        let sandbox_provider =
            if settings.dry_run_enabled() && !matches!(sandbox_provider, SandboxProvider::Local) {
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
            .map(|value| value.parse::<Provider>())
            .transpose()
            .map_err(|err| FabroError::Precondition(err.to_string()))?
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
                run_id: Some(record.run_id.clone()),
                clone_branch: detected_base_branch.or_else(|| record.base_branch.clone()),
            },
            #[cfg(feature = "exedev")]
            SandboxProvider::Exe => SandboxSpec::Exe {
                config: resolve_exe_config(&settings).unwrap_or_default(),
                clone_params: resolve_exe_clone_params(&working_directory),
                run_id: Some(record.run_id.clone()),
                github_app: services.github_app.clone(),
                mgmt_destination: "exe.dev".to_string(),
            },
            #[cfg(not(feature = "exedev"))]
            SandboxProvider::Exe => {
                return Err(FabroError::Precondition(
                    "exe sandbox requires the exedev feature".to_string(),
                ));
            }
            SandboxProvider::Ssh => SandboxSpec::Ssh {
                config: resolve_ssh_config(&settings).ok_or_else(|| {
                    FabroError::Precondition(
                        "--sandbox ssh requires [sandbox.ssh] config".to_string(),
                    )
                })?,
                clone_params: resolve_ssh_clone_params(&working_directory),
                run_id: Some(record.run_id.clone()),
                github_app: services.github_app.clone(),
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

        Ok(RunSession {
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
            git_author: services.git_author,
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
        .map(|provider| provider.parse::<SandboxProvider>())
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

fn resolve_daytona_config(
    settings: &FabroSettings,
) -> Option<fabro_sandbox::daytona::DaytonaConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.daytona.clone())
}

#[cfg(feature = "exedev")]
fn resolve_exe_config(settings: &FabroSettings) -> Option<fabro_sandbox::exe::ExeConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.exe.clone())
}

#[cfg(feature = "exedev")]
fn resolve_exe_clone_params(cwd: &Path) -> Option<fabro_sandbox::exe::GitCloneParams> {
    let (detected_url, branch) = match fabro_sandbox::daytona::detect_repo_info(cwd) {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!("No git repo detected for exe.dev clone: {err}");
            return None;
        }
    };
    let url = fabro_github::ssh_url_to_https(&detected_url);
    Some(fabro_sandbox::exe::GitCloneParams { url, branch })
}

fn resolve_ssh_config(settings: &FabroSettings) -> Option<fabro_sandbox::ssh::SshConfig> {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.ssh.clone())
}

fn resolve_ssh_clone_params(cwd: &Path) -> Option<fabro_sandbox::ssh::GitCloneParams> {
    let (detected_url, branch) = match fabro_sandbox::daytona::detect_repo_info(cwd) {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!("No git repo detected for SSH clone: {err}");
            return None;
        }
    };
    let url = fabro_github::ssh_url_to_https(&detected_url);
    Some(fabro_sandbox::ssh::GitCloneParams { url, branch })
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

        let record = persisted.run_record();
        let run_options = RunOptions {
            settings: record.settings.clone(),
            run_dir: persisted.run_dir().to_path_buf(),
            cancel_token: self.cancel_token,
            run_id: record.run_id.clone(),
            labels: record.labels.clone(),
            git_author: self.git_author,
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

        ProgressLogger::new(persisted.run_dir(), record.run_id.clone())
            .register(self.emitter.as_ref());

        let init_options = InitOptions {
            run_id: record.run_id.clone(),
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
        let initialized = pipeline::initialize(persisted, init_options).await?;

        let sandbox_for_cleanup = Arc::clone(&initialized.sandbox);
        let cleanup_guard = scopeguard::guard((), move |()| {
            if preserve_sandbox {
                return;
            }
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = sandbox_for_cleanup.cleanup().await;
                });
            }
        });

        let executed = pipeline::execute(initialized).await;
        let failed = !matches!(
            executed.outcome.as_ref().map(|outcome| &outcome.status),
            Ok(StageStatus::Success) | Ok(StageStatus::PartialSuccess)
        );

        let retro_opts = RetroOptions {
            run_id: executed.run_options.run_id.clone(),
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
            run_id: retroed.run_options.run_id.clone(),
            workflow_name: retroed.graph.name.clone(),
            hook_runner: retroed.hook_runner.clone(),
            preserve_sandbox: self.preserve_sandbox,
            last_git_sha: last_git_sha.lock().unwrap().clone(),
        };
        let pr_opts = PullRequestOptions {
            run_dir: retroed.run_options.run_dir.clone(),
            pr_config: self.pr_config,
            github_app: self.pr_github_app,
            origin_url: self.pr_origin_url,
            model: self.pr_model,
        };

        let retro = retroed.retro.clone();
        let concluded = pipeline::finalize(retroed, &finalize_opts).await?;
        let finalized = pipeline::pull_request(concluded, &pr_opts).await;

        scopeguard::ScopeGuard::into_inner(cleanup_guard);

        Ok(Started {
            finalized,
            retro,
            retro_duration,
        })
    }
}

struct DetachedRunBootstrapGuard {
    run_dir: PathBuf,
    active: bool,
}

impl DetachedRunBootstrapGuard {
    fn arm(run_dir: &Path) -> Result<Self, FabroError> {
        run_status::write_run_status(
            run_dir,
            RunStatus::Starting,
            Some(StatusReason::SandboxInitializing),
        );
        Ok(Self {
            run_dir: run_dir.to_path_buf(),
            active: true,
        })
    }

    fn defuse(&mut self) {
        self.active = false;
    }
}

impl Drop for DetachedRunBootstrapGuard {
    fn drop(&mut self) {
        if self.active {
            run_status::write_run_status(
                &self.run_dir,
                RunStatus::Failed,
                Some(StatusReason::SandboxInitFailed),
            );
        }
    }
}

const POSTRUN_ABORTED_MESSAGE: &str = "Run aborted before post-run finalization completed.";

struct DetachedRunCompletionGuard {
    run_dir: PathBuf,
    active: bool,
}

impl DetachedRunCompletionGuard {
    fn arm(run_dir: &Path) -> Self {
        Self {
            run_dir: run_dir.to_path_buf(),
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

        run_status::write_run_status(
            &self.run_dir,
            RunStatus::Failed,
            Some(StatusReason::WorkflowError),
        );
        if !self.run_dir.join("conclusion.json").exists() {
            let _ = write_failure_conclusion(
                &self.run_dir,
                POSTRUN_ABORTED_MESSAGE,
                Some(StatusReason::WorkflowError),
            );
        }
        if let Some(run_id) = load_run_id(&self.run_dir) {
            let _ = crate::event::append_progress_event(
                &self.run_dir,
                &run_id,
                &WorkflowRunEvent::RunNotice {
                    level: RunNoticeLevel::Error,
                    code: "postrun_aborted".to_string(),
                    message: POSTRUN_ABORTED_MESSAGE.to_string(),
                },
            );
        }
    }
}

fn load_run_id(run_dir: &Path) -> Option<String> {
    crate::records::RunRecord::load(run_dir)
        .ok()
        .map(|record| record.run_id)
        .filter(|run_id| !run_id.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string(run_dir.join("id.txt"))
                .ok()
                .map(|run_id| run_id.trim().to_string())
                .filter(|run_id| !run_id.is_empty())
        })
}

fn persist_detached_failure(
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

    write_failure_conclusion(run_dir, &message, Some(reason))?;
    run_status::write_run_status(run_dir, RunStatus::Failed, Some(reason));

    if let Some(run_id) = load_run_id(run_dir) {
        crate::event::append_progress_event(
            run_dir,
            &run_id,
            &WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Error,
                code: format!("{phase}_failed"),
                message,
            },
        )
        .map_err(|err| FabroError::Io(err.to_string()))?;
    }

    Ok(())
}

fn write_failure_conclusion(
    run_dir: &Path,
    message: &str,
    _reason: Option<StatusReason>,
) -> Result<(), FabroError> {
    if run_dir.join("conclusion.json").exists() {
        return Ok(());
    }

    let conclusion = Conclusion {
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
    };
    conclusion.save(&run_dir.join("conclusion.json"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    use chrono::Utc;
    use fabro_config::FabroSettings;

    use super::*;
    use crate::context::Context;
    use crate::event::EventEmitter;
    use crate::handler::exit::ExitHandler;
    use crate::handler::start::StartHandler;
    use crate::handler::HandlerRegistry;
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
            run_id: Some("run-test".to_string()),
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

    fn test_start_services(
        emitter: Arc<EventEmitter>,
        registry: Arc<HandlerRegistry>,
    ) -> StartServices {
        StartServices {
            cancel_token: None,
            emitter,
            interviewer: Arc::new(fabro_interview::AutoApproveInterviewer),
            git_author: crate::git::GitAuthor::default(),
            github_app: None,
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
        let started = start(&run_dir, test_start_services(emitter, registry))
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

        let started = start(&run_dir, test_start_services(emitter, registry))
            .await
            .unwrap();

        assert_eq!(started.finalized.conclusion.status, StageStatus::Success);
        assert!(run_dir.join("conclusion.json").exists());
    }

    #[tokio::test]
    async fn start_errors_when_checkpoint_exists() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());

        persisted_workflow(MINIMAL_DOT, &run_dir);
        std::fs::write(run_dir.join("checkpoint.json"), "{}").unwrap();

        let result = start(&run_dir, test_start_services(emitter, registry)).await;

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

        let result = resume(&run_dir, test_start_services(emitter, registry)).await;

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

        let result = resume(&run_dir, test_start_services(emitter, registry)).await;

        assert!(
            matches!(&result, Err(crate::error::FabroError::Precondition(_))),
            "expected Precondition error, got: {result:?}",
            result = result.as_ref().map(|_| "Ok"),
        );
    }
}
