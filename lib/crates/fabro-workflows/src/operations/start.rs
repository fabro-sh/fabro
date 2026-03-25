use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::FabroError;
use crate::event::WorkflowRunEvent;
use crate::outcome::StageStatus;
use crate::pipeline::{self, FinalizeOptions, Finalized, InitOptions, RetroOptions, Validated};

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

/// Run a validated workflow through initialize, execute, retro, and finalize.
pub async fn start(validated: Validated, options: StartOptions) -> Result<Started, FabroError> {
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

    let initialized = pipeline::initialize(validated, options.init).await?;

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
