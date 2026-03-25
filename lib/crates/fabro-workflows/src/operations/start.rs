use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::FabroError;
use crate::event::WorkflowRunEvent;
use crate::outcome::StageStatus;
use crate::pipeline::{self, FinalizeOptions, Finalized, InitOptions, Persisted, RetroOptions};

pub struct StartRetroConfig {
    pub enabled: bool,
    pub dry_run: bool,
    pub llm_client: Option<fabro_llm::client::Client>,
    pub provider: fabro_llm::Provider,
    pub model: String,
}

pub struct StartFinalizeConfig {
    pub preserve_sandbox: bool,
    pub pr_config: Option<fabro_config::run::PullRequestConfig>,
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    pub origin_url: Option<String>,
    pub model: String,
}

pub struct StartOptions {
    pub init: InitOptions,
    pub retro: StartRetroConfig,
    pub finalize: StartFinalizeConfig,
}

pub struct Started {
    pub finalized: Finalized,
    pub retro: Option<fabro_retro::retro::Retro>,
    pub retro_duration: Duration,
}

/// Run a persisted workflow through initialize, execute, retro, and finalize.
pub async fn start(persisted: Persisted, options: StartOptions) -> Result<Started, FabroError> {
    let preserve_sandbox = options.finalize.preserve_sandbox;
    let sandbox_for_cleanup = Arc::clone(&options.init.sandbox);
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

    let initialized = pipeline::initialize(persisted, options.init).await?;

    let last_git_sha: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let sha_clone = Arc::clone(&last_git_sha);
        initialized.emitter.on_event(move |event| {
            if let WorkflowRunEvent::CheckpointCompleted {
                git_commit_sha: Some(sha),
                ..
            } = event
            {
                *sha_clone.lock().unwrap() = Some(sha.clone());
            }
        });
    }

    let executed = pipeline::execute(initialized).await;
    let failed = !matches!(
        executed.outcome.as_ref().map(|outcome| &outcome.status),
        Ok(StageStatus::Success) | Ok(StageStatus::PartialSuccess)
    );

    let retro_opts = RetroOptions {
        run_id: executed.settings.run_id.clone(),
        workflow_name: executed.graph.name.clone(),
        goal: executed.graph.goal().to_string(),
        run_dir: executed.settings.run_dir.clone(),
        sandbox: Arc::clone(&executed.sandbox),
        emitter: Some(Arc::clone(&executed.emitter)),
        failed,
        run_duration_ms: executed.duration_ms,
        enabled: options.retro.enabled,
        dry_run: options.retro.dry_run,
        llm_client: options.retro.llm_client,
        provider: options.retro.provider,
        model: options.retro.model,
    };

    let retro_start = Instant::now();
    let retroed = pipeline::retro(executed, &retro_opts).await;
    let retro_duration = retro_start.elapsed();

    let finalize_opts = FinalizeOptions {
        run_dir: retroed.settings.run_dir.clone(),
        run_id: retroed.settings.run_id.clone(),
        workflow_name: retroed.graph.name.clone(),
        hook_runner: retroed.hook_runner.clone(),
        preserve_sandbox: options.finalize.preserve_sandbox,
        pr_config: options.finalize.pr_config,
        github_app: options.finalize.github_app,
        origin_url: options.finalize.origin_url,
        model: options.finalize.model,
        last_git_sha: last_git_sha.lock().unwrap().clone(),
    };

    let retro = retroed.retro.clone();
    let finalized = pipeline::finalize(retroed, &finalize_opts).await?;

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
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use chrono::Utc;
    use fabro_agent::{DirEntry, ExecResult, GrepOptions, LocalSandbox, Sandbox};
    use fabro_config::config::FabroConfig;
    use fabro_graphviz::graph::{Graph, Node};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::context::Context;
    use crate::event::EventEmitter;
    use crate::handler::exit::ExitHandler;
    use crate::handler::start::StartHandler;
    use crate::handler::{Handler, HandlerRegistry};
    use crate::outcome::Outcome;
    use crate::pipeline::PersistOptions;
    use crate::records::RunRecord;
    use crate::run_settings::{LifecycleConfig, RunSettings};

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    const EMIT_DOT: &str = r#"digraph Test {
        graph [goal="Ship feature"]
        start [shape=Mdiamond]
        work [type="emit"]
        exit [shape=Msquare]
        start -> work -> exit
    }"#;

    struct CleanupCountingSandbox {
        inner: Arc<dyn Sandbox>,
        cleanup_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Sandbox for CleanupCountingSandbox {
        async fn read_file(
            &self,
            path: &str,
            offset: Option<usize>,
            limit: Option<usize>,
        ) -> Result<String, String> {
            self.inner.read_file(path, offset, limit).await
        }

        async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
            self.inner.write_file(path, content).await
        }

        async fn delete_file(&self, path: &str) -> Result<(), String> {
            self.inner.delete_file(path).await
        }

        async fn file_exists(&self, path: &str) -> Result<bool, String> {
            self.inner.file_exists(path).await
        }

        async fn list_directory(
            &self,
            path: &str,
            depth: Option<usize>,
        ) -> Result<Vec<DirEntry>, String> {
            self.inner.list_directory(path, depth).await
        }

        async fn exec_command(
            &self,
            command: &str,
            timeout_ms: u64,
            working_dir: Option<&str>,
            env_vars: Option<&HashMap<String, String>>,
            cancel_token: Option<CancellationToken>,
        ) -> Result<ExecResult, String> {
            self.inner
                .exec_command(command, timeout_ms, working_dir, env_vars, cancel_token)
                .await
        }

        async fn grep(
            &self,
            pattern: &str,
            path: &str,
            options: &GrepOptions,
        ) -> Result<Vec<String>, String> {
            self.inner.grep(pattern, path, options).await
        }

        async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String> {
            self.inner.glob(pattern, path).await
        }

        async fn download_file_to_local(
            &self,
            remote_path: &str,
            local_path: &Path,
        ) -> Result<(), String> {
            self.inner
                .download_file_to_local(remote_path, local_path)
                .await
        }

        async fn upload_file_from_local(
            &self,
            local_path: &Path,
            remote_path: &str,
        ) -> Result<(), String> {
            self.inner
                .upload_file_from_local(local_path, remote_path)
                .await
        }

        async fn initialize(&self) -> Result<(), String> {
            self.inner.initialize().await
        }

        async fn cleanup(&self) -> Result<(), String> {
            self.cleanup_count.fetch_add(1, Ordering::SeqCst);
            self.inner.cleanup().await
        }

        fn working_directory(&self) -> &str {
            self.inner.working_directory()
        }

        fn platform(&self) -> &str {
            self.inner.platform()
        }

        fn os_version(&self) -> String {
            self.inner.os_version()
        }

        fn sandbox_info(&self) -> String {
            self.inner.sandbox_info()
        }

        async fn refresh_push_credentials(&self) -> Result<(), String> {
            self.inner.refresh_push_credentials().await
        }

        async fn set_autostop_interval(&self, minutes: i32) -> Result<(), String> {
            self.inner.set_autostop_interval(minutes).await
        }

        async fn setup_git_for_run(
            &self,
            run_id: &str,
        ) -> Result<Option<fabro_sandbox::GitRunInfo>, String> {
            self.inner.setup_git_for_run(run_id).await
        }

        fn resume_setup_commands(&self, run_branch: &str) -> Vec<String> {
            self.inner.resume_setup_commands(run_branch)
        }

        async fn git_push_branch(&self, branch: &str) -> bool {
            self.inner.git_push_branch(branch).await
        }

        fn host_git_dir(&self) -> Option<&str> {
            self.inner.host_git_dir()
        }

        fn parallel_worktree_path(
            &self,
            run_dir: &Path,
            run_id: &str,
            node_id: &str,
            key: &str,
        ) -> String {
            self.inner
                .parallel_worktree_path(run_dir, run_id, node_id, key)
        }

        async fn ssh_access_command(&self) -> Result<Option<String>, String> {
            self.inner.ssh_access_command().await
        }

        fn origin_url(&self) -> Option<&str> {
            self.inner.origin_url()
        }

        async fn get_preview_url(
            &self,
            port: u16,
        ) -> Result<Option<(String, HashMap<String, String>)>, String> {
            self.inner.get_preview_url(port).await
        }

        fn mark_agent_read(&self, path: &str) {
            self.inner.mark_agent_read(path);
        }
    }

    struct EmitCheckpointHandler;

    #[async_trait]
    impl Handler for EmitCheckpointHandler {
        async fn execute(
            &self,
            node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            services: &crate::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            services
                .emitter
                .emit(&WorkflowRunEvent::CheckpointCompleted {
                    node_id: node.id.clone(),
                    status: "success".to_string(),
                    git_commit_sha: Some("sha-test".to_string()),
                });
            Ok(Outcome::success())
        }
    }

    fn persisted_workflow(dot: &str, run_dir: &std::path::Path) -> Persisted {
        let validated =
            crate::operations::create(dot, crate::operations::CreateOptions::default()).unwrap();
        validated.raise_on_errors().unwrap();
        let graph = validated.graph().clone();
        crate::pipeline::persist(
            validated,
            PersistOptions {
                run_dir: run_dir.to_path_buf(),
                run_record: RunRecord {
                    run_id: "run-test".to_string(),
                    created_at: Utc::now(),
                    config: FabroConfig::default(),
                    graph,
                    workflow_slug: Some("test".to_string()),
                    working_directory: std::env::current_dir().unwrap(),
                    host_repo_path: Some(std::env::current_dir().unwrap().display().to_string()),
                    base_branch: Some("main".to_string()),
                    labels: HashMap::new(),
                },
            },
        )
        .unwrap()
    }

    fn test_settings(run_dir: &std::path::Path) -> RunSettings {
        RunSettings {
            config: FabroConfig::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "run-test".to_string(),
            labels: HashMap::new(),
            git_author: crate::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            host_repo_path: None,
            base_branch: None,
            git: None,
        }
    }

    fn test_registry() -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register("emit", Box::new(EmitCheckpointHandler));
        registry
    }

    fn test_start_options(
        run_dir: &std::path::Path,
        sandbox: Arc<dyn Sandbox>,
        emitter: Arc<EventEmitter>,
        registry: Arc<HandlerRegistry>,
        lifecycle: LifecycleConfig,
        preserve_sandbox: bool,
    ) -> StartOptions {
        StartOptions {
            init: InitOptions {
                run_id: "run-test".to_string(),
                dry_run: false,
                emitter,
                sandbox,
                registry,
                lifecycle,
                run_settings: test_settings(run_dir),
                hooks: fabro_hooks::HookConfig { hooks: vec![] },
                sandbox_env: HashMap::new(),
                checkpoint: None,
                seed_context: None,
            },
            retro: StartRetroConfig {
                enabled: false,
                dry_run: false,
                llm_client: None,
                provider: fabro_llm::Provider::Anthropic,
                model: "test-model".to_string(),
            },
            finalize: StartFinalizeConfig {
                preserve_sandbox,
                pr_config: None,
                github_app: None,
                origin_url: None,
                model: "test-model".to_string(),
            },
        }
    }

    fn counting_sandbox() -> (Arc<dyn Sandbox>, Arc<AtomicUsize>) {
        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let inner: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));
        (
            Arc::new(CleanupCountingSandbox {
                inner,
                cleanup_count: Arc::clone(&cleanup_count),
            }),
            cleanup_count,
        )
    }

    #[tokio::test]
    async fn start_cleans_up_sandbox_when_initialize_fails() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());
        let (sandbox, cleanup_count) = counting_sandbox();

        let result = start(
            persisted_workflow(MINIMAL_DOT, &run_dir),
            test_start_options(
                &run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleConfig {
                    setup_commands: vec!["false".to_string()],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                false,
            ),
        )
        .await;

        assert!(result.is_err());
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn start_captures_checkpoint_git_sha_in_conclusion() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));

        let started = start(
            persisted_workflow(EMIT_DOT, &run_dir),
            test_start_options(
                &run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleConfig {
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
    async fn start_runs_loaded_persisted_workflow() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let wrong_run_dir = temp.path().join("wrong-run-dir");
        let emitter = Arc::new(EventEmitter::new());
        let registry = Arc::new(test_registry());
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()));

        persisted_workflow(MINIMAL_DOT, &run_dir);
        let loaded = Persisted::load(&run_dir).unwrap();

        let started = start(
            loaded,
            test_start_options(
                &wrong_run_dir,
                sandbox,
                emitter,
                registry,
                LifecycleConfig {
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
        assert!(!wrong_run_dir.join("conclusion.json").exists());
    }
}
