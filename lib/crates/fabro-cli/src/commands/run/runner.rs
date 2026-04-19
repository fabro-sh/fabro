use std::io::{BufRead as StdBufRead, BufReader as StdBufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use fabro_config::Storage;
use fabro_interview::{ControlInterviewer, WorkerControlEnvelope, WorkerControlMessage};
use fabro_store::{EventEnvelope, EventPayload, RunProjection};
use fabro_types::settings::run::RunMode;
use fabro_types::settings::{InterpString, SettingsLayer};
use fabro_types::{EventBody, RunBlobId, RunEvent, RunId, StatusReason};
use fabro_vault::Vault;
use fabro_workflow::artifact_snapshot::CapturedArtifactInfo;
use fabro_workflow::artifact_upload::{ArtifactSink, StageArtifactUploader};
use fabro_workflow::event::{Emitter, RunEventSink};
use fabro_workflow::operations::{self, BlockedStateTracker, StartServices};
use fabro_workflow::run_control::RunControlState;
use fabro_workflow::runtime_store::{RunStoreBackend, RunStoreHandle};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Mutex, RwLock as AsyncRwLock, mpsc};
use tokio::time::sleep;

use crate::args::RunWorkerMode;
use crate::server_client;
use crate::shared::github::build_github_credentials;

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
    storage_dir: Option<PathBuf>,
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
    let artifact_sink = Some(ArtifactSink::Uploader(build_artifact_uploader(
        run_id,
        client.clone_for_reuse(),
        artifact_upload_token,
    )));
    let interviewer = Arc::new(ControlInterviewer::new());
    let cancel_token = Arc::new(AtomicBool::new(false));
    spawn_worker_control_stream(Arc::clone(&interviewer), Arc::clone(&cancel_token))?;
    let run_control = RunControlState::new();
    install_signal_handlers(Arc::clone(&run_control), Arc::clone(&cancel_token))?;
    let vault = load_worker_vault(storage_dir.as_deref())?;
    let github_app = {
        let vault_guard = match &vault {
            Some(arc) => Some(arc.read().await),
            None => None,
        };
        maybe_build_github_credentials(&run_record.settings, vault_guard.as_deref())?
    };
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
        artifact_sink,
        run_control: Some(run_control),
        blocked_state_tracker: Some(BlockedStateTracker::new()),
        github_app,
        vault,
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

fn load_worker_vault(storage_dir: Option<&Path>) -> Result<Option<Arc<AsyncRwLock<Vault>>>> {
    let Some(storage_dir) = storage_dir else {
        return Ok(None);
    };

    let storage = Storage::new(storage_dir);
    let vault = Vault::load(storage.secrets_path()).with_context(|| {
        format!(
            "failed to load worker vault from {}",
            storage.root().display()
        )
    })?;
    Ok(Some(Arc::new(AsyncRwLock::new(vault))))
}

#[derive(Debug, PartialEq, Eq)]
enum WorkerControlStreamEvent {
    Line(String),
    Eof,
}

#[expect(
    clippy::disallowed_methods,
    reason = "Worker control reads blocking stdin on a dedicated OS thread and forwards lines into Tokio."
)]
fn spawn_worker_control_stream(
    interviewer: Arc<ControlInterviewer>,
    cancel_token: Arc<AtomicBool>,
) -> Result<()> {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    tokio::spawn(handle_worker_control_stream_events(
        interviewer,
        cancel_token,
        event_rx,
    ));
    std::thread::Builder::new()
        .name("fabro-worker-control".to_string())
        .spawn(move || {
            read_worker_control_stream_blocking(StdBufReader::new(std::io::stdin()), &event_tx);
        })
        .context("failed to spawn worker control reader thread")?;
    Ok(())
}

fn read_worker_control_stream_blocking<R>(
    mut reader: R,
    event_tx: &mpsc::UnboundedSender<WorkerControlStreamEvent>,
) where
    R: StdBufRead,
{
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => {
                let _ = event_tx.send(WorkerControlStreamEvent::Eof);
                break;
            }
            Ok(_) => {
                let line = line.trim_end_matches(['\r', '\n']).to_string();
                if event_tx.send(WorkerControlStreamEvent::Line(line)).is_err() {
                    break;
                }
            }
        }
    }
}

async fn handle_worker_control_stream_events(
    interviewer: Arc<ControlInterviewer>,
    cancel_token: Arc<AtomicBool>,
    mut event_rx: mpsc::UnboundedReceiver<WorkerControlStreamEvent>,
) {
    while let Some(event) = event_rx.recv().await {
        match event {
            WorkerControlStreamEvent::Line(line) => {
                apply_worker_control_line(&interviewer, &cancel_token, &line).await;
            }
            WorkerControlStreamEvent::Eof => {
                interviewer.interrupt_all().await;
                return;
            }
        }
    }

    interviewer.interrupt_all().await;
}

async fn apply_worker_control_line(
    interviewer: &ControlInterviewer,
    cancel_token: &AtomicBool,
    line: &str,
) {
    if line.trim().is_empty() {
        return;
    }

    let Ok(message) = serde_json::from_str::<WorkerControlEnvelope>(line) else {
        return;
    };

    match message.message {
        WorkerControlMessage::InterviewAnswer { qid, answer } => {
            let _ = interviewer.submit(&qid, answer.into()).await;
        }
        WorkerControlMessage::RunCancel => {
            cancel_token.store(true, Ordering::SeqCst);
            interviewer.interrupt_all().await;
        }
    }
}

fn build_artifact_uploader(
    run_id: RunId,
    client: server_client::ServerStoreClient,
    artifact_upload_token: Option<String>,
) -> Arc<dyn StageArtifactUploader> {
    match artifact_upload_token {
        Some(token) => Arc::new(HttpArtifactUploader {
            run_id,
            client,
            bearer_token: token,
        }),
        None => Arc::new(MissingArtifactUploadTokenUploader { run_id }),
    }
}

struct HttpArtifactUploader {
    run_id:       RunId,
    client:       server_client::ServerStoreClient,
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
            "run {} could not upload artifacts because the worker did not receive an artifact upload token",
            self.run_id
        ))
    }
}

#[derive(Clone)]
struct HttpRunStore {
    run_id: RunId,
    client: server_client::ServerStoreClient,
    state:  Arc<Mutex<RunProjection>>,
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

fn maybe_build_github_credentials(
    settings: &SettingsLayer,
    vault: Option<&fabro_vault::Vault>,
) -> Result<Option<fabro_github::GitHubCredentials>> {
    let resolved_run = fabro_config::resolve_run_from_file(settings).ok();
    let resolved_server = fabro_config::resolve_server_from_file(settings).ok();
    let required_github_credentials = resolved_run.as_ref().is_some_and(|settings| {
        settings.execution.mode != RunMode::DryRun && settings.sandbox.provider == "daytona"
    }) || resolved_server
        .as_ref()
        .is_some_and(|settings| !settings.integrations.github.permissions.is_empty());
    let pull_request_enabled = resolved_run.as_ref().is_some_and(|settings| {
        settings.execution.mode != RunMode::DryRun && settings.pull_request.is_some()
    });
    let strategy = resolved_server
        .as_ref()
        .map(|settings| settings.integrations.github.strategy)
        .unwrap_or_default();
    let app_id = resolved_server
        .as_ref()
        .and_then(|settings| settings.integrations.github.app_id.as_ref())
        .map(InterpString::as_source);

    if required_github_credentials {
        return build_github_credentials(strategy, app_id.as_deref(), vault);
    }

    if pull_request_enabled {
        return Ok(build_github_credentials(strategy, app_id.as_deref(), vault)
            .ok()
            .flatten());
    }

    Ok(None)
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
#[allow(clippy::absolute_paths)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use fabro_auth::{AuthCredential, AuthDetails};
    use fabro_config::Storage;
    use fabro_interview::{AnswerValue, ControlInterviewer, Interviewer, Question, QuestionType};
    use fabro_model::Provider;
    use fabro_types::run_event::{
        InterviewCompletedProps, InterviewStartedProps, RunCompletedProps, RunControlEffectProps,
        RunFailedProps, RunStatusTransitionProps,
    };
    use fabro_types::{EventBody, StatusReason, fixtures};
    use fabro_vault::{SecretType, Vault};
    use fabro_workflow::artifact_upload::StageArtifactUploader;

    use super::{
        MissingArtifactUploadTokenUploader, WorkerControlStreamEvent, WorkerTitlePhase,
        apply_worker_control_line, handle_worker_control_stream_events, initial_worker_title_phase,
        load_worker_vault, read_worker_control_stream_blocking, worker_title,
        worker_title_phase_for_event,
    };
    use crate::args::RunWorkerMode;

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
                question_id:     "q-1".to_string(),
                question:        "Approve?".to_string(),
                stage:           "gate".to_string(),
                question_type:   "yes_no".to_string(),
                options:         Vec::new(),
                allow_freeform:  false,
                timeout_seconds: None,
                context_display: None,
            })),
            Some(WorkerTitlePhase::Waiting)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::InterviewCompleted(InterviewCompletedProps {
                question_id: "q-1".to_string(),
                question:    "Approve?".to_string(),
                answer:      "yes".to_string(),
                duration_ms: 10,
            })),
            Some(WorkerTitlePhase::Running)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunCompleted(RunCompletedProps {
                duration_ms:          10,
                artifact_count:       0,
                status:               "success".to_string(),
                reason:               None,
                total_usd_micros:     None,
                final_git_commit_sha: None,
                final_patch:          None,
                billing:              None,
            })),
            Some(WorkerTitlePhase::Succeeded)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunFailed(RunFailedProps {
                error:          "cancelled".to_string(),
                duration_ms:    10,
                reason:         Some(StatusReason::Cancelled),
                git_commit_sha: None,
            })),
            Some(WorkerTitlePhase::Cancelled)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunFailed(RunFailedProps {
                error:          "boom".to_string(),
                duration_ms:    10,
                reason:         Some(StatusReason::Terminated),
                git_commit_sha: None,
            })),
            Some(WorkerTitlePhase::Failed)
        );
    }

    #[tokio::test]
    async fn missing_artifact_upload_token_error_does_not_mention_removed_storage_mode() {
        let uploader = MissingArtifactUploadTokenUploader {
            run_id: fixtures::RUN_1,
        };
        let temp = tempfile::tempdir().unwrap();

        let error = uploader
            .upload_stage_artifacts(&fabro_types::StageId::new("code", 2), temp.path(), &[])
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("worker did not receive an artifact upload token")
        );
        assert!(!error.to_string().contains("object-backed artifacts"));
    }

    #[tokio::test]
    async fn worker_control_line_routes_answer_by_question_id() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = Arc::new(AtomicBool::new(false));
        let mut question = Question::new("Approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let ask_interviewer = Arc::clone(&interviewer);
        let answer_task = tokio::spawn(async move { ask_interviewer.ask(question).await });

        apply_worker_control_line(
            &interviewer,
            &cancel_token,
            r#"{"v":1,"type":"interview.answer","qid":"q-1","answer":{"kind":"yes"}}"#,
        )
        .await;

        let answer: fabro_interview::Answer = answer_task.await.unwrap();
        assert_eq!(answer.value, AnswerValue::Yes);
        assert!(!cancel_token.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn worker_control_line_cancel_sets_cancel_token_and_interrupts_pending_interviews() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = Arc::new(AtomicBool::new(false));
        let mut question = Question::new("Approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let ask_interviewer = Arc::clone(&interviewer);
        let answer_task = tokio::spawn(async move { ask_interviewer.ask(question).await });
        tokio::task::yield_now().await;

        apply_worker_control_line(
            &interviewer,
            &cancel_token,
            r#"{"v":1,"type":"run.cancel"}"#,
        )
        .await;

        let answer: fabro_interview::Answer = answer_task.await.unwrap();
        assert_eq!(answer.value, AnswerValue::Interrupted);
        assert!(cancel_token.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn blocking_worker_control_stream_emits_lines_and_eof() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        read_worker_control_stream_blocking(
            std::io::Cursor::new(
                b"{\"v\":1,\"type\":\"run.cancel\"}\n{\"v\":1,\"type\":\"interview.answer\",\"qid\":\"q-1\",\"answer\":{\"kind\":\"yes\"}}\n",
            ),
            &event_tx,
        );

        assert_eq!(
            event_rx.try_recv(),
            Ok(WorkerControlStreamEvent::Line(
                r#"{"v":1,"type":"run.cancel"}"#.to_string()
            ))
        );
        assert_eq!(
            event_rx.try_recv(),
            Ok(WorkerControlStreamEvent::Line(
                r#"{"v":1,"type":"interview.answer","qid":"q-1","answer":{"kind":"yes"}}"#
                    .to_string()
            ))
        );
        assert_eq!(event_rx.try_recv(), Ok(WorkerControlStreamEvent::Eof));
    }

    #[tokio::test]
    async fn worker_control_event_loop_eof_interrupts_pending_interviews() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = Arc::new(AtomicBool::new(false));
        let mut question = Question::new("Approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let ask_interviewer = Arc::clone(&interviewer);
        let answer_task = tokio::spawn(async move { ask_interviewer.ask(question).await });
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

        event_tx.send(WorkerControlStreamEvent::Eof).unwrap();
        drop(event_tx);

        handle_worker_control_stream_events(
            Arc::clone(&interviewer),
            Arc::clone(&cancel_token),
            event_rx,
        )
        .await;

        let answer: fabro_interview::Answer = answer_task.await.unwrap();
        assert_eq!(answer.value, AnswerValue::Interrupted);
        assert!(!cancel_token.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn load_worker_vault_reads_credentials_from_storage_dir() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::new(temp.path());
        let mut vault = Vault::load(storage.secrets_path()).unwrap();
        vault
            .set(
                "anthropic",
                &serde_json::to_string(&AuthCredential {
                    provider: Provider::Anthropic,
                    details:  AuthDetails::ApiKey {
                        key: "vault-key".to_string(),
                    },
                })
                .unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();

        let loaded = load_worker_vault(Some(temp.path())).unwrap().unwrap();
        let guard = loaded.read().await;
        let credential = guard.get("anthropic").unwrap();

        assert!(credential.contains("vault-key"));
    }
}
