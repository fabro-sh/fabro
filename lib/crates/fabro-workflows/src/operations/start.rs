use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::context::Context;
use crate::error::FabroError;
use crate::event::{EventEmitter, ProgressLogger, WorkflowRunEvent};
use crate::outcome::StageStatus;
use crate::pipeline::{
    self, DevcontainerSpec, FinalizeOptions, Finalized, InitOptions, LlmSpec, Persisted,
    PullRequestOptions, RetroOptions, SandboxEnvSpec, SandboxSpec,
};
use crate::records::{Checkpoint, Conclusion};
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions};
use fabro_config::sandbox::WorktreeMode;
use fabro_interview::Interviewer;

pub struct StartRetroOptions {
    pub enabled: bool,
}

pub struct StartFinalizeOptions {
    pub preserve_sandbox: bool,
}

pub struct StartPullRequestConfig {
    pub pr_config: Option<fabro_config::run::PullRequestConfig>,
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    pub origin_url: Option<String>,
    pub model: String,
}

/// Options for `start()` and `resume()`.
///
/// Fields that are derivable from `RunRecord` (run_id, labels, base_branch,
/// host_repo_path, config, workflow_slug) are read from disk by `run_engine()`.
/// Callers only provide truly external values.
pub struct StartOptions {
    // Truly external (not derivable from RunRecord)
    pub cancel_token: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: SandboxSpec,
    pub llm: LlmSpec,
    pub interviewer: Arc<dyn Interviewer>,
    pub lifecycle: LifecycleOptions,
    pub hooks: fabro_hooks::HookConfig,
    pub sandbox_env: SandboxEnvSpec,
    pub devcontainer: Option<DevcontainerSpec>,
    pub seed_context: Option<Context>,
    pub git_author: crate::git::GitAuthor,
    pub git: Option<GitCheckpointOptions>,
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    pub worktree_mode: Option<WorktreeMode>,
    #[cfg(test)]
    pub registry_override: Option<Arc<crate::handler::HandlerRegistry>>,

    // Still external for now — could be derived from RunRecord.config in follow-up
    pub dry_run: bool,
    pub retro: StartRetroOptions,
    pub finalize: StartFinalizeOptions,
    pub pull_request: StartPullRequestConfig,
}

pub struct Started {
    pub finalized: Finalized,
    pub retro: Option<fabro_retro::retro::Retro>,
    pub retro_duration: Duration,
}

/// Start a fresh workflow run. Errors if a checkpoint already exists (use `resume()` instead).
pub async fn start(
    run_dir: &std::path::Path,
    options: StartOptions,
) -> Result<Started, FabroError> {
    if run_dir.join("checkpoint.json").exists() {
        return Err(FabroError::Precondition(
            "checkpoint.json exists in run directory — did you mean to resume?".to_string(),
        ));
    }
    let persisted = Persisted::load(run_dir)?;
    run_engine(persisted, None, options).await
}

/// Resume a workflow run from its checkpoint. Errors if no checkpoint is found.
pub async fn resume(
    run_dir: &std::path::Path,
    options: StartOptions,
) -> Result<Started, FabroError> {
    if let Ok(record) = crate::run_status::RunStatusRecord::load(&run_dir.join("status.json")) {
        if record.status == crate::run_status::RunStatus::Succeeded {
            return Err(FabroError::Precondition(
                "run already finished successfully — nothing to resume".to_string(),
            ));
        }
    }
    if let Ok(conclusion) = Conclusion::load(&run_dir.join("conclusion.json")) {
        if matches!(
            conclusion.status,
            StageStatus::Success | StageStatus::PartialSuccess | StageStatus::Skipped
        ) {
            return Err(FabroError::Precondition(
                "run already finished successfully — nothing to resume".to_string(),
            ));
        }
    }
    let cp_path = run_dir.join("checkpoint.json");
    let checkpoint = Checkpoint::load(&cp_path)
        .map_err(|e| FabroError::Precondition(format!("no checkpoint to resume from: {e}")))?;
    let persisted = Persisted::load(run_dir)?;
    run_engine(persisted, Some(checkpoint), options).await
}

/// Shared engine: initialize, execute, retro, finalize, pull_request.
async fn run_engine(
    persisted: Persisted,
    checkpoint: Option<Checkpoint>,
    options: StartOptions,
) -> Result<Started, FabroError> {
    let preserve_sandbox = options.finalize.preserve_sandbox;

    // Build RunOptions from the persisted RunRecord + external caller options
    let record = persisted.run_record();
    let run_options = RunOptions {
        config: record.config.clone(),
        run_dir: persisted.run_dir().to_path_buf(),
        cancel_token: options.cancel_token,
        dry_run: options.dry_run,
        run_id: record.run_id.clone(),
        labels: record.labels.clone(),
        git_author: options.git_author,
        workflow_slug: record.workflow_slug.clone(),
        github_app: options.github_app.clone(),
        host_repo_path: record
            .host_repo_path
            .as_deref()
            .map(std::path::PathBuf::from),
        base_branch: record.base_branch.clone(),
        display_base_sha: None,
        git: None,
    };

    let last_git_sha: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let sha_clone = Arc::clone(&last_git_sha);
        options.emitter.on_event(move |event| match event {
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
        .register(options.emitter.as_ref());

    let init_options = InitOptions {
        run_id: record.run_id.clone(),
        dry_run: options.dry_run,
        emitter: options.emitter,
        sandbox: options.sandbox,
        llm: options.llm,
        interviewer: options.interviewer,
        lifecycle: options.lifecycle,
        run_options,
        hooks: options.hooks,
        sandbox_env: options.sandbox_env,
        devcontainer: options.devcontainer,
        git: options.git,
        worktree_mode: options.worktree_mode,
        #[cfg(test)]
        registry_override: options.registry_override,
        checkpoint,
        seed_context: options.seed_context,
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
        enabled: options.retro.enabled,
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
        preserve_sandbox: options.finalize.preserve_sandbox,
        last_git_sha: last_git_sha.lock().unwrap().clone(),
    };
    let pr_opts = PullRequestOptions {
        run_dir: retroed.run_options.run_dir.clone(),
        pr_config: options.pull_request.pr_config,
        github_app: options.pull_request.github_app,
        origin_url: options.pull_request.origin_url,
        model: options.pull_request.model,
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    use chrono::Utc;
    use fabro_agent::{LocalSandbox, Sandbox};
    use fabro_config::config::FabroConfig;

    use super::*;
    use crate::context::Context;
    use crate::event::EventEmitter;
    use crate::handler::exit::ExitHandler;
    use crate::handler::start::StartHandler;
    use crate::handler::HandlerRegistry;
    use crate::pipeline::{LlmSpec, SandboxEnvSpec, SandboxSpec};
    use crate::run_options::LifecycleOptions;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    fn persisted_workflow(dot: &str, run_dir: &std::path::Path) -> Persisted {
        crate::operations::create(
            dot,
            crate::operations::RunCreateOptions {
                config: FabroConfig::default(),
                run_dir: Some(run_dir.to_path_buf()),
                run_id: Some("run-test".to_string()),
                workflow_slug: Some("test".to_string()),
                labels: std::collections::HashMap::new(),
                base_branch: Some("main".to_string()),
                working_directory: Some(std::env::current_dir().unwrap()),
                host_repo_path: Some(std::env::current_dir().unwrap().display().to_string()),
                goal_override: None,
                base_dir: None,
            },
        )
        .unwrap()
    }

    fn test_registry() -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry
    }

    fn test_start_options(
        _run_dir: &std::path::Path,
        _sandbox: Arc<dyn Sandbox>,
        emitter: Arc<EventEmitter>,
        registry: Arc<HandlerRegistry>,
        lifecycle: LifecycleOptions,
        preserve_sandbox: bool,
    ) -> StartOptions {
        StartOptions {
            cancel_token: None,
            emitter,
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model: "test-model".to_string(),
                provider: fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers: Vec::new(),
                dry_run: true,
            },
            interviewer: Arc::new(fabro_interview::AutoApproveInterviewer),
            lifecycle,
            hooks: fabro_hooks::HookConfig { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                devcontainer_env: HashMap::new(),
                toml_env: HashMap::new(),
                github_permissions: None,
                origin_url: None,
            },
            devcontainer: None,
            seed_context: None,
            git_author: crate::git::GitAuthor::default(),
            git: None,
            github_app: None,
            worktree_mode: None,
            registry_override: Some(registry),
            dry_run: false,
            retro: StartRetroOptions { enabled: false },
            finalize: StartFinalizeOptions { preserve_sandbox },
            pull_request: StartPullRequestConfig {
                pr_config: None,
                github_app: None,
                origin_url: None,
                model: "test-model".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn start_cleans_up_sandbox_when_initialize_fails() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));

        persisted_workflow(MINIMAL_DOT, &run_dir);
        let result = start(
            &run_dir,
            test_start_options(
                &run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleOptions {
                    setup_commands: vec!["false".to_string()],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                false,
            ),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn start_captures_checkpoint_git_sha_in_conclusion() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));
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
            test_start_options(
                &run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleOptions {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                true,
            ),
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
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));

        persisted_workflow(MINIMAL_DOT, &run_dir);

        let started = start(
            &run_dir,
            test_start_options(
                &run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleOptions {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                true,
            ),
        )
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
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));

        persisted_workflow(MINIMAL_DOT, &run_dir);
        // Create a fake checkpoint file
        std::fs::write(run_dir.join("checkpoint.json"), "{}").unwrap();

        let result = start(
            &run_dir,
            test_start_options(
                &run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleOptions {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                false,
            ),
        )
        .await;

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
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));

        persisted_workflow(MINIMAL_DOT, &run_dir);

        let result = resume(
            &run_dir,
            test_start_options(
                &run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleOptions {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                false,
            ),
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
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));

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
            test_start_options(
                &run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleOptions {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                false,
            ),
        )
        .await;

        assert!(
            matches!(&result, Err(crate::error::FabroError::Precondition(_))),
            "expected Precondition error, got: {result:?}",
            result = result.as_ref().map(|_| "Ok"),
        );
    }
}
