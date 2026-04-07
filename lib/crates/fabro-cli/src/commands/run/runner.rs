use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use fabro_interview::{
    ControlInterviewer, InterviewBroker, WorkerControlEnvelope, WorkerControlMessage,
};
use fabro_store::{EventEnvelope, EventPayload, RunProjection};
use fabro_types::{EventBody, RunBlobId, RunEvent, RunId, Settings, StatusReason};
use fabro_workflow::artifact_snapshot::CapturedArtifactInfo;
use fabro_workflow::artifact_upload::StageArtifactUploader;
use fabro_workflow::event::{Emitter, RunEventSink};
use fabro_workflow::operations::{self, StartServices};
use fabro_workflow::run_control::RunControlState;
use fabro_workflow::runtime_store::{RunStoreBackend, RunStoreHandle};
use tokio::io::{self, AsyncBufReadExt, AsyncRead, BufReader};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::args::RunWorkerMode;
use crate::server_client;
use crate::shared::github::build_github_app_credentials;

const RUN_STORE_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(250),
];

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
    artifact_upload_token: Option<String>,
    run_dir: PathBuf,
    mode: RunWorkerMode,
) -> Result<()> {
    let _ = fabro_proc::title_init();
    set_worker_title(&run_id, initial_worker_title_phase(mode));

    let client = server_client::connect_server_target_direct(&server).await?;
    let run_store = HttpRunStore::connect(run_id, client.clone_for_reuse()).await?;
    let run_state = run_store
        .state()
        .await
        .with_context(|| format!("failed to load run state for {run_id}"))?;
    let run_record = run_state
        .run
        .as_ref()
        .ok_or_else(|| anyhow!("Run {run_id} has no run record in store"))?;
    let artifact_uploader = build_artifact_uploader(
        run_id,
        run_record,
        client.clone_for_reuse(),
        artifact_upload_token,
    );
    let broker = Arc::new(InterviewBroker::new());
    let interviewer = Arc::new(ControlInterviewer::new(Arc::clone(&broker)));
    tokio::spawn(read_worker_control_stream(io::stdin(), broker));
    let run_control = RunControlState::new();
    let cancel_token = Arc::new(AtomicBool::new(false));
    install_signal_handlers(Arc::clone(&run_control), Arc::clone(&cancel_token))?;
    let github_app = maybe_build_github_app_credentials(&run_record.settings)?;
    let services = StartServices {
        run_id,
        cancel_token: Some(Arc::clone(&cancel_token)),
        emitter: Arc::new(Emitter::new(run_id)),
        interviewer,
        run_store: run_store.clone(),
        event_sink: RunEventSink::fanout(vec![
            RunEventSink::backend(run_store),
            RunEventSink::callback(move |event| {
                update_worker_title_from_event(&event);
                async move { Ok(()) }
            }),
        ]),
        artifact_uploader,
        run_control: Some(run_control),
        github_app,
        on_node: None,
        registry_override: None,
    };

    match mode {
        RunWorkerMode::Start => {
            operations::start(&run_dir, services).await?;
        }
        RunWorkerMode::Resume => {
            operations::resume(&run_dir, services).await?;
        }
    }

    Ok(())
}

async fn read_worker_control_stream<R>(reader: R, broker: Arc<InterviewBroker>)
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                apply_worker_control_line(&broker, &line).await;
            }
            Ok(None) | Err(_) => {
                broker.abort_all().await;
                break;
            }
        }
    }
}

async fn apply_worker_control_line(broker: &InterviewBroker, line: &str) {
    if line.trim().is_empty() {
        return;
    }

    let Ok(message) = serde_json::from_str::<WorkerControlEnvelope>(line) else {
        return;
    };

    match message.message {
        WorkerControlMessage::InterviewAnswer { qid, answer } => {
            let _ = broker.submit(&qid, answer.into()).await;
        }
    }
}

fn build_artifact_uploader(
    run_id: RunId,
    run_record: &fabro_types::RunRecord,
    client: server_client::ServerStoreClient,
    artifact_upload_token: Option<String>,
) -> Option<Arc<dyn StageArtifactUploader>> {
    if !run_record.uses_object_backed_artifacts() {
        return None;
    }

    let uploader: Arc<dyn StageArtifactUploader> = match artifact_upload_token {
        Some(token) => Arc::new(HttpArtifactUploader {
            run_id,
            client,
            bearer_token: token,
        }),
        None => Arc::new(MissingArtifactUploadTokenUploader { run_id }),
    };

    Some(uploader)
}

struct HttpArtifactUploader {
    run_id: RunId,
    client: server_client::ServerStoreClient,
    bearer_token: String,
}

#[async_trait]
impl StageArtifactUploader for HttpArtifactUploader {
    async fn upload_stage_artifacts(
        &self,
        stage_id: &fabro_types::StageId,
        artifact_capture_dir: &Path,
        artifacts: &[CapturedArtifactInfo],
    ) -> Result<()> {
        if artifacts.is_empty() {
            return Ok(());
        }

        if artifacts.len() == 1 {
            let artifact = &artifacts[0];
            return self
                .client
                .upload_stage_artifact_file(
                    &self.run_id,
                    stage_id,
                    &artifact.path,
                    &artifact_capture_dir.join(&artifact.path),
                    &self.bearer_token,
                )
                .await;
        }

        self.client
            .upload_stage_artifact_batch(
                &self.run_id,
                stage_id,
                artifact_capture_dir,
                artifacts,
                &self.bearer_token,
            )
            .await
    }
}

struct MissingArtifactUploadTokenUploader {
    run_id: RunId,
}

#[async_trait]
impl StageArtifactUploader for MissingArtifactUploadTokenUploader {
    async fn upload_stage_artifacts(
        &self,
        _stage_id: &fabro_types::StageId,
        _artifact_capture_dir: &Path,
        _artifacts: &[CapturedArtifactInfo],
    ) -> Result<()> {
        Err(anyhow!(
            "run {} is configured for object-backed artifacts but the worker did not receive an artifact upload token",
            self.run_id
        ))
    }
}

#[derive(Clone)]
struct HttpRunStore {
    run_id: RunId,
    client: server_client::ServerStoreClient,
    state: Arc<Mutex<RunProjection>>,
    events: Arc<Mutex<Option<Vec<EventEnvelope>>>>,
}

impl HttpRunStore {
    async fn connect(
        run_id: RunId,
        client: server_client::ServerStoreClient,
    ) -> Result<RunStoreHandle> {
        let state = client
            .get_run_state(&run_id)
            .await
            .with_context(|| format!("failed to fetch run state for {run_id}"))?;
        Ok(RunStoreHandle::new(Arc::new(Self {
            run_id,
            client,
            state: Arc::new(Mutex::new(state)),
            events: Arc::new(Mutex::new(None)),
        })))
    }

    async fn with_retries<T, F, Fut>(&self, operation: &'static str, mut op: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut last_error = None;
        for attempt in 0..=RUN_STORE_RETRY_DELAYS.len() {
            match op().await {
                Ok(value) => return Ok(value),
                Err(err) => last_error = Some(err),
            }
            if let Some(delay) = RUN_STORE_RETRY_DELAYS.get(attempt) {
                sleep(*delay).await;
            }
        }
        Err(last_error
            .unwrap_or_else(|| anyhow!("run store operation failed"))
            .context(format!(
                "worker lost canonical run store during {operation}"
            )))
    }

    async fn refresh_state_from_server(&self) -> Result<RunProjection> {
        self.with_retries("refresh state", || {
            let client = self.client.clone_for_reuse();
            let run_id = self.run_id;
            async move { client.get_run_state(&run_id).await }
        })
        .await
    }

    async fn apply_acknowledged_event(&self, seq: u32, event: &RunEvent) -> Result<()> {
        let payload = EventPayload::new(event.to_value()?, &self.run_id)?;
        let envelope = EventEnvelope { seq, payload };

        {
            let mut state = self.state.lock().await;
            if let Err(err) = state.apply_event(&envelope) {
                tracing::warn!(run_id = %self.run_id, error = %err, "failed to apply acknowledged event to local run-state mirror; refreshing from server");
                drop(state);
                let refreshed = self.refresh_state_from_server().await?;
                *self.state.lock().await = refreshed;
            }
        }

        let mut events = self.events.lock().await;
        if let Some(cached) = events.as_mut() {
            cached.push(envelope);
        }

        Ok(())
    }
}

#[async_trait]
impl RunStoreBackend for HttpRunStore {
    async fn load_state(&self) -> Result<RunProjection> {
        Ok(self.state.lock().await.clone())
    }

    async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        let mut cached = self.events.lock().await;
        if let Some(events) = cached.as_ref() {
            return Ok(events.clone());
        }

        let events = self
            .with_retries("list run events", || {
                let client = self.client.clone_for_reuse();
                let run_id = self.run_id;
                async move { client.list_run_events(&run_id, None, None).await }
            })
            .await?;
        *cached = Some(events.clone());
        Ok(events)
    }

    async fn append_run_event(&self, event: &RunEvent) -> Result<()> {
        let seq = self
            .with_retries("append run event", || {
                let client = self.client.clone_for_reuse();
                let run_id = self.run_id;
                let event = event.clone();
                async move { client.append_run_event(&run_id, &event).await }
            })
            .await?;
        self.apply_acknowledged_event(seq, event).await
    }

    async fn write_blob(&self, data: &[u8]) -> Result<RunBlobId> {
        self.with_retries("write run blob", || {
            let client = self.client.clone_for_reuse();
            let run_id = self.run_id;
            let data = data.to_vec();
            async move { client.write_run_blob(&run_id, &data).await }
        })
        .await
    }

    async fn read_blob(&self, id: &RunBlobId) -> Result<Option<bytes::Bytes>> {
        self.with_retries("read run blob", || {
            let client = self.client.clone_for_reuse();
            let run_id = self.run_id;
            let blob_id = *id;
            async move { client.read_run_blob(&run_id, &blob_id).await }
        })
        .await
    }
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
    use std::sync::Arc;

    use httpmock::MockServer;
    use serde_json::json;

    use super::{
        WorkerTitlePhase, apply_worker_control_line, execute, initial_worker_title_phase,
        read_worker_control_stream, worker_title, worker_title_phase_for_event,
    };
    use crate::args::RunWorkerMode;
    use fabro_interview::{AnswerValue, InterviewBroker};
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
                question_id: "q-1".to_string(),
                question: "Approve?".to_string(),
                stage: "gate".to_string(),
                question_type: "yes_no".to_string(),
                options: Vec::new(),
                allow_freeform: false,
                timeout_seconds: None,
                context_display: None,
            })),
            Some(WorkerTitlePhase::Waiting)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::InterviewCompleted(InterviewCompletedProps {
                question_id: "q-1".to_string(),
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
                total_usd_micros: None,
                final_git_commit_sha: None,
                final_patch: None,
                billing: None,
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

    #[tokio::test]
    async fn worker_bootstrap_loads_run_state_without_prefetching_run_events() {
        let server = MockServer::start_async().await;
        let run_id = fixtures::RUN_1;

        let state_mock = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path(format!("/api/v1/runs/{run_id}/state"));
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        json!({
                            "run": null,
                            "graph_source": null,
                            "start": null,
                            "status": null,
                            "checkpoint": null,
                            "checkpoints": [],
                            "conclusion": null,
                            "retro": null,
                            "retro_prompt": null,
                            "retro_response": null,
                            "sandbox": null,
                            "final_patch": null,
                            "pull_request": null,
                            "nodes": {}
                        })
                        .to_string(),
                    );
            })
            .await;
        let events_mock = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path(format!("/api/v1/runs/{run_id}/events"));
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(json!({ "data": [], "meta": { "has_more": false } }).to_string());
            })
            .await;

        let run_dir = tempfile::tempdir().unwrap();
        let error = execute(
            run_id,
            format!("{}/api/v1", server.base_url()),
            None,
            run_dir.path().to_path_buf(),
            RunWorkerMode::Start,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("has no run record"));
        state_mock.assert_async().await;
        assert_eq!(events_mock.calls_async().await, 0);
    }

    #[tokio::test]
    async fn worker_control_line_routes_answer_by_question_id() {
        let broker = Arc::new(InterviewBroker::new());
        let receiver = broker.register("q-1".to_string()).await;

        apply_worker_control_line(
            &broker,
            r#"{"v":1,"type":"interview.answer","qid":"q-1","answer":{"kind":"yes"}}"#,
        )
        .await;

        let answer: fabro_interview::Answer = receiver.await.unwrap();
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn worker_control_stream_eof_aborts_pending_interviews() {
        let broker = Arc::new(InterviewBroker::new());
        let receiver = broker.register("q-1".to_string()).await;

        read_worker_control_stream(tokio::io::empty(), Arc::clone(&broker)).await;

        let answer: fabro_interview::Answer = receiver.await.unwrap();
        assert_eq!(answer.value, AnswerValue::Aborted);
    }
}
