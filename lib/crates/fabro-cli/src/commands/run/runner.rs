use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use fabro_config::RunScratch;
use fabro_interview::FileInterviewer;
use fabro_store::{Database, EventPayload, RunDatabase};
use fabro_types::{EventBody, RunEvent, RunId, Settings, StatusReason};
use fabro_workflow::event::{Emitter, RunEventSink};
use fabro_workflow::run_control::RunControlState;
use object_store::memory::InMemory as MemoryObjectStore;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};

use crate::args::RunWorkerMode;
use crate::server_client;
use crate::shared::github::build_github_app_credentials;

const STORE_FLUSH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerTitlePhase {
    Start,
    Resume,
    Init,
    Running,
    Waiting,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

pub(crate) async fn execute(
    run_id: RunId,
    server: String,
    run_dir: PathBuf,
    mode: RunWorkerMode,
) -> Result<()> {
    let _ = fabro_proc::title_init();
    set_worker_title(&run_id, initial_worker_title_phase(mode));

    let client = server_client::connect_server_target_direct(&server).await?;
    let run_store = load_seed_run_store(&client, &run_id).await?;
    let run_state = run_store
        .state()
        .await
        .with_context(|| format!("failed to load run state for {run_id}"))?;
    let run_record = run_state
        .run
        .as_ref()
        .ok_or_else(|| anyhow!("Run {run_id} has no run record in store"))?;
    let scratch = RunScratch::new(&run_dir);
    let interviewer = Arc::new(FileInterviewer::new(
        scratch.interview_request_path(),
        scratch.interview_response_path(),
        scratch.interview_claim_path(),
    ));
    let run_control = RunControlState::new();
    let cancel_token = Arc::new(AtomicBool::new(false));
    install_signal_handlers(Arc::clone(&run_control), Arc::clone(&cancel_token))?;
    let github_app = maybe_build_github_app_credentials(&run_record.settings)?;
    let event_client = client.clone_for_reuse();
    let services = fabro_workflow::operations::StartServices {
        run_id,
        cancel_token: Some(Arc::clone(&cancel_token)),
        emitter: Arc::new(Emitter::new(run_id)),
        interviewer,
        run_store: run_store.clone(),
        event_sink: RunEventSink::fanout(vec![
            RunEventSink::store(run_store),
            RunEventSink::callback(move |event| {
                update_worker_title_from_event(&event);
                let client = event_client.clone_for_reuse();
                async move { client.append_run_event(&event.run_id, &event).await }
            }),
        ]),
        run_control: Some(run_control),
        github_app,
        on_node: None,
        registry_override: None,
    };

    match mode {
        RunWorkerMode::Start => {
            fabro_workflow::operations::start(&run_dir, services).await?;
        }
        RunWorkerMode::Resume => {
            fabro_workflow::operations::resume(&run_dir, services).await?;
        }
    }

    Ok(())
}

fn open_memory_store() -> Arc<Database> {
    Arc::new(Database::new(
        Arc::new(MemoryObjectStore::new()),
        "",
        STORE_FLUSH_INTERVAL,
    ))
}

async fn load_seed_run_store(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
) -> Result<RunDatabase> {
    let events = client
        .list_run_events(run_id, None, None)
        .await
        .with_context(|| format!("failed to fetch run events for {run_id}"))?;
    let payloads = events
        .into_iter()
        .map(|event| event.payload)
        .collect::<Vec<_>>();
    seed_run_store(run_id, &payloads).await
}

async fn seed_run_store(run_id: &RunId, events: &[EventPayload]) -> Result<RunDatabase> {
    let store = open_memory_store();
    let run_store = store
        .create_run(run_id)
        .await
        .with_context(|| format!("failed to create in-memory run store for {run_id}"))?;
    for payload in events {
        run_store
            .append_event(payload)
            .await
            .with_context(|| format!("failed to seed in-memory run store for {run_id}"))?;
    }
    Ok(run_store)
}

fn set_worker_title(run_id: &RunId, phase: WorkerTitlePhase) {
    fabro_proc::title_set(&worker_title(run_id, phase));
}

fn initial_worker_title_phase(mode: RunWorkerMode) -> WorkerTitlePhase {
    match mode {
        RunWorkerMode::Start => WorkerTitlePhase::Start,
        RunWorkerMode::Resume => WorkerTitlePhase::Resume,
    }
}

fn worker_title(run_id: &RunId, phase: WorkerTitlePhase) -> String {
    let short_id: String = run_id.to_string().chars().take(12).collect();
    let phase = match phase {
        WorkerTitlePhase::Start => "start",
        WorkerTitlePhase::Resume => "resume",
        WorkerTitlePhase::Init => "init",
        WorkerTitlePhase::Running => "running",
        WorkerTitlePhase::Waiting => "waiting",
        WorkerTitlePhase::Paused => "paused",
        WorkerTitlePhase::Succeeded => "succeeded",
        WorkerTitlePhase::Failed => "failed",
        WorkerTitlePhase::Cancelled => "cancelled",
    };
    format!("fabro {short_id} {phase}")
}

fn worker_title_phase_for_event(body: &EventBody) -> Option<WorkerTitlePhase> {
    match body {
        EventBody::RunStarting(_) => Some(WorkerTitlePhase::Init),
        EventBody::RunRunning(_) | EventBody::RunUnpaused(_) => Some(WorkerTitlePhase::Running),
        EventBody::InterviewStarted(_) => Some(WorkerTitlePhase::Waiting),
        EventBody::InterviewCompleted(_) | EventBody::InterviewTimeout(_) => {
            Some(WorkerTitlePhase::Running)
        }
        EventBody::RunPaused(_) => Some(WorkerTitlePhase::Paused),
        EventBody::RunCompleted(_) => Some(WorkerTitlePhase::Succeeded),
        EventBody::RunFailed(props) => Some(if props.reason == Some(StatusReason::Cancelled) {
            WorkerTitlePhase::Cancelled
        } else {
            WorkerTitlePhase::Failed
        }),
        _ => None,
    }
}

fn update_worker_title_from_event(event: &RunEvent) {
    if let Some(phase) = worker_title_phase_for_event(&event.body) {
        set_worker_title(&event.run_id, phase);
    }
}

fn maybe_build_github_app_credentials(
    settings: &Settings,
) -> Result<Option<fabro_github::GitHubAppCredentials>> {
    let needs_github_app = settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.provider.as_deref())
        .is_some_and(|provider| provider == "daytona")
        || settings
            .pull_request
            .as_ref()
            .is_some_and(|pull_request| pull_request.enabled)
        || settings.github_permissions().is_some();

    if needs_github_app {
        build_github_app_credentials(settings.app_id())
    } else {
        Ok(None)
    }
}

fn install_signal_handlers(
    run_control: Arc<RunControlState>,
    cancel_token: Arc<AtomicBool>,
) -> Result<()> {
    #[cfg(unix)]
    {
        let mut pause = signal(SignalKind::user_defined1())?;
        let pause_control = Arc::clone(&run_control);
        tokio::spawn(async move {
            while pause.recv().await.is_some() {
                pause_control.request_pause();
            }
        });

        let mut unpause = signal(SignalKind::user_defined2())?;
        tokio::spawn(async move {
            while unpause.recv().await.is_some() {
                run_control.request_unpause();
            }
        });

        let mut terminate = signal(SignalKind::terminate())?;
        let terminate_cancel = Arc::clone(&cancel_token);
        tokio::spawn(async move {
            while terminate.recv().await.is_some() {
                terminate_cancel.store(true, Ordering::SeqCst);
            }
        });

        let mut interrupt = signal(SignalKind::interrupt())?;
        tokio::spawn(async move {
            while interrupt.recv().await.is_some() {
                cancel_token.store(true, Ordering::SeqCst);
            }
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        WorkerTitlePhase, initial_worker_title_phase, worker_title, worker_title_phase_for_event,
    };
    use crate::args::RunWorkerMode;
    use fabro_types::fixtures;
    use fabro_types::run_event::{
        InterviewCompletedProps, InterviewStartedProps, RunCompletedProps, RunControlEffectProps,
        RunFailedProps, RunStatusTransitionProps,
    };
    use fabro_types::{EventBody, StatusReason};

    #[test]
    fn worker_title_uses_short_run_id_and_phase() {
        let short_id: String = fixtures::RUN_1.to_string().chars().take(12).collect();
        assert_eq!(
            worker_title(&fixtures::RUN_1, WorkerTitlePhase::Start),
            format!("fabro {short_id} start")
        );
        assert_eq!(
            worker_title(&fixtures::RUN_1, WorkerTitlePhase::Succeeded),
            format!("fabro {short_id} succeeded")
        );
    }

    #[test]
    fn initial_worker_title_phase_matches_mode() {
        assert_eq!(
            initial_worker_title_phase(RunWorkerMode::Start),
            WorkerTitlePhase::Start
        );
        assert_eq!(
            initial_worker_title_phase(RunWorkerMode::Resume),
            WorkerTitlePhase::Resume
        );
    }

    #[test]
    fn worker_title_phase_tracks_lifecycle_events() {
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunStarting(RunStatusTransitionProps {
                reason: None,
            })),
            Some(WorkerTitlePhase::Init)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunPaused(RunControlEffectProps::default())),
            Some(WorkerTitlePhase::Paused)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::InterviewStarted(InterviewStartedProps {
                question: "Approve?".to_string(),
                question_type: "yes_no".to_string(),
            })),
            Some(WorkerTitlePhase::Waiting)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::InterviewCompleted(InterviewCompletedProps {
                question: "Approve?".to_string(),
                answer: "yes".to_string(),
                duration_ms: 10,
            })),
            Some(WorkerTitlePhase::Running)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunCompleted(RunCompletedProps {
                duration_ms: 10,
                artifact_count: 0,
                status: "success".to_string(),
                reason: None,
                total_cost: None,
                final_git_commit_sha: None,
                final_patch: None,
                usage: None,
            })),
            Some(WorkerTitlePhase::Succeeded)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunFailed(RunFailedProps {
                error: "cancelled".to_string(),
                duration_ms: 10,
                reason: Some(StatusReason::Cancelled),
                git_commit_sha: None,
            })),
            Some(WorkerTitlePhase::Cancelled)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunFailed(RunFailedProps {
                error: "boom".to_string(),
                duration_ms: 10,
                reason: Some(StatusReason::Terminated),
                git_commit_sha: None,
            })),
            Some(WorkerTitlePhase::Failed)
        );
    }
}
