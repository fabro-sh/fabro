use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use fabro_client::ServerTarget;
use fabro_config::Storage;
use fabro_interview::{
    AnswerSubmission, ControlInterviewer, WORKER_CONTROL_INVALID_CURSOR_REASON,
    WORKER_CONTROL_PONG_TIMEOUT_REASON, WORKER_CONTROL_WS_LIVENESS_TIMEOUT,
    WORKER_CONTROL_WS_PING_INTERVAL, WorkerControlAck, WorkerControlDeliveryFrame,
    WorkerControlEnvelope, WorkerControlMessage,
};
use fabro_manifest::SuppliedWorkflowVersionPackager;
use fabro_petri::controls::RunControls;
use fabro_tool::fabro_client::ClientBackend;
use fabro_types::RunId;
use fabro_vault::{SecretStore, Vault};
use fabro_workflow::services::FabroRunToolServices;
use futures::{SinkExt, StreamExt};
use jsonwebtoken::dangerous::insecure_decode;
#[cfg(test)]
use tokio::io::DuplexStream;
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{RwLock as AsyncRwLock, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Request as WebSocketRequest, header};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{self, Message as WebSocketMessage};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite};
use tokio_util::sync::CancellationToken;

use super::petri_worker::{self, PetriControls, PetriWorker};
use crate::args::RunWorkerMode;
use crate::server_client;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerTitlePhase {
    Start,
    Resume,
    Running,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

pub(crate) async fn execute(
    run_id: RunId,
    server: String,
    storage_dir: PathBuf,
    run_dir: PathBuf,
    mode: RunWorkerMode,
    fabro_home: Option<PathBuf>,
    worker_token: &str,
) -> Result<()> {
    let _ = fabro_proc::title_init();
    set_worker_title(&run_id, initial_worker_title_phase(mode));

    let target = server.parse::<ServerTarget>()?;
    let client = server_client::connect_server_target_with_bearer(&target, worker_token).await?;
    let run_state = client
        .get_run_state(&run_id)
        .await
        .with_context(|| format!("failed to load run state for {run_id}"))?;
    if matches!(mode, RunWorkerMode::Resume) && run_state.status.is_terminal() {
        let how = match run_state.status {
            fabro_types::RunStatus::Succeeded { .. } => "successfully",
            _ => "already",
        };
        anyhow::bail!("Precondition failed: run already finished {how} — nothing to resume");
    }
    Box::pin(petri_worker::execute(PetriWorker {
        run_id,
        target,
        client,
        run_state,
        storage_dir: &storage_dir,
        run_dir,
        mode,
        fabro_home,
        worker_token,
    }))
    .await
}

const WORKER_TOKEN_SCOPE: &str = "run:worker";
const WORKER_RUN_TOOLS_SCOPE: &str = "agent:run_tools";

#[derive(serde::Deserialize)]
struct WorkerTokenScopeClaim {
    scope: String,
}

pub(super) fn fabro_run_tools_enabled_from_worker_token(worker_token: &str) -> bool {
    // Local tool registration only. The server validates the token signature and
    // scopes.
    insecure_decode::<WorkerTokenScopeClaim>(worker_token)
        .is_ok_and(|token| worker_scope_has_run_tools(&token.claims.scope))
}

fn worker_scope_has_run_tools(scope_claim: &str) -> bool {
    let mut has_run_worker = false;
    let mut has_agent_run_tools = false;
    for scope in scope_claim.split_whitespace() {
        match scope {
            WORKER_TOKEN_SCOPE => has_run_worker = true,
            WORKER_RUN_TOOLS_SCOPE => has_agent_run_tools = true,
            _ => return false,
        }
    }
    has_run_worker && has_agent_run_tools
}

pub(super) fn build_fabro_run_tool_services(
    worker_token: &str,
    client: fabro_client::Client,
    current_run_id: RunId,
) -> Option<FabroRunToolServices> {
    if worker_token.trim().is_empty() {
        return None;
    }
    let backend = ClientBackend::new(Arc::new(client))
        .with_workflow_version_packager(Arc::new(SuppliedWorkflowVersionPackager));
    Some(FabroRunToolServices {
        backend: Arc::new(backend),
        current_run_id,
    })
}

/// Load the worker's secret vault from the run's storage root.
///
/// A worker always receives the server storage root so it can load the same
/// secret vault as the server.
pub(super) async fn load_worker_vault(storage_dir: &Path) -> Result<Arc<AsyncRwLock<Vault>>> {
    let storage = Storage::new(storage_dir);
    let vault = SecretStore::open_snapshot(storage.sqlite_path(), storage.secrets_path())
        .await
        .with_context(|| {
            format!(
                "failed to load worker secrets from {}",
                storage.root().display()
            )
        })?
        .into_vault();
    Ok(Arc::new(AsyncRwLock::new(vault)))
}

const WORKER_CONTROL_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const WORKER_CONTROL_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(5);
const WORKER_CONTROL_APPLIED_ID_DEDUPE_CAPACITY: usize = 2048;

#[derive(Default)]
struct AppliedWorkerControlDeliveryIds {
    last:   Option<String>,
    order:  VecDeque<String>,
    recent: HashSet<String>,
}

impl AppliedWorkerControlDeliveryIds {
    fn last_applied_id(&self) -> Option<&str> {
        self.last.as_deref()
    }

    fn contains(&self, id: &str) -> bool {
        self.recent.contains(id)
    }

    fn record(&mut self, id: String) {
        if !self.recent.insert(id.clone()) {
            self.last = Some(id);
            return;
        }
        self.order.push_back(id.clone());
        self.last = Some(id);
        while self.order.len() > WORKER_CONTROL_APPLIED_ID_DEDUPE_CAPACITY {
            if let Some(evicted) = self.order.pop_front() {
                self.recent.remove(&evicted);
            }
        }
    }
}

/// Where the run's pause, unpause and steer controls go: the Petri run's
/// controls. Cancel and answers are applied by the channel itself.
pub(super) type WorkerControls = Arc<PetriControls>;

pub(super) struct WorkerControlManagerHandle {
    first_connection: Option<oneshot::Receiver<Result<()>>>,
    fatal:            Option<oneshot::Receiver<anyhow::Error>>,
    done:             CancellationToken,
    task:             JoinHandle<()>,
}

impl WorkerControlManagerHandle {
    pub(super) async fn wait_for_first_connection(&mut self) -> Result<()> {
        let receiver = self
            .first_connection
            .take()
            .context("worker control first-connection receiver missing")?;
        receiver
            .await
            .context("worker control manager stopped before first connection")?
    }

    pub(super) async fn fatal_control_loss(&mut self) -> anyhow::Error {
        let Some(receiver) = self.fatal.take() else {
            return anyhow!("worker control fatal receiver missing");
        };
        receiver
            .await
            .unwrap_or_else(|_| anyhow!("worker control manager stopped before workflow completed"))
    }

    pub(super) fn finish(&self) {
        self.done.cancel();
        self.task.abort();
    }
}

#[derive(Debug)]
struct WorkerControlStreamConnectRequest {
    request:          WebSocketRequest<()>,
    unix_socket_path: Option<PathBuf>,
}

impl WorkerControlStreamConnectRequest {
    fn request_for_tungstenite(&self) -> WebSocketRequest<()> {
        self.request.clone()
    }

    #[cfg(test)]
    fn uri(&self) -> String {
        self.request.uri().to_string()
    }

    #[cfg(test)]
    fn authorization(&self) -> Option<&str> {
        self.request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
    }
}

enum WorkerControlSocket {
    Tcp(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
    #[cfg(unix)]
    Unix(Box<WebSocketStream<UnixStream>>),
    #[cfg(test)]
    Test(Box<WebSocketStream<DuplexStream>>),
}

impl WorkerControlSocket {
    async fn send(&mut self, message: WebSocketMessage) -> Result<(), tungstenite::Error> {
        match self {
            Self::Tcp(socket) => socket.send(message).await,
            #[cfg(unix)]
            Self::Unix(socket) => socket.send(message).await,
            #[cfg(test)]
            Self::Test(socket) => socket.send(message).await,
        }
    }

    async fn next(&mut self) -> Option<Result<WebSocketMessage, tungstenite::Error>> {
        match self {
            Self::Tcp(socket) => socket.next().await,
            #[cfg(unix)]
            Self::Unix(socket) => socket.next().await,
            #[cfg(test)]
            Self::Test(socket) => socket.next().await,
        }
    }
}

#[derive(Debug)]
enum WorkerControlConnectError {
    InvalidCursor,
    Other(anyhow::Error),
}

pub(super) fn spawn_worker_control_manager(
    target: ServerTarget,
    run_id: RunId,
    worker_token: String,
    interviewer: Arc<ControlInterviewer>,
    cancel_token: CancellationToken,
    controls: WorkerControls,
) -> WorkerControlManagerHandle {
    let (first_tx, first_rx) = oneshot::channel();
    let (fatal_tx, fatal_rx) = oneshot::channel();
    let done = CancellationToken::new();
    let task_done = done.clone();
    let task = tokio::spawn(async move {
        run_worker_control_manager(
            target,
            run_id,
            worker_token,
            interviewer,
            cancel_token,
            controls,
            task_done,
            first_tx,
            fatal_tx,
        )
        .await;
    });
    WorkerControlManagerHandle {
        first_connection: Some(first_rx),
        fatal: Some(fatal_rx),
        done,
        task,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Worker control manager owns the worker-side control dependencies."
)]
async fn run_worker_control_manager(
    target: ServerTarget,
    run_id: RunId,
    worker_token: String,
    interviewer: Arc<ControlInterviewer>,
    cancel_token: CancellationToken,
    controls: WorkerControls,
    done: CancellationToken,
    first_tx: oneshot::Sender<Result<()>>,
    fatal_tx: oneshot::Sender<anyhow::Error>,
) {
    let mut first_tx = Some(first_tx);
    let mut fatal_tx = Some(fatal_tx);
    let mut backoff = WORKER_CONTROL_RECONNECT_INITIAL_BACKOFF;
    let mut applied_ids = AppliedWorkerControlDeliveryIds::default();

    while !done.is_cancelled() {
        let request = match build_worker_control_stream_request(
            &target,
            &run_id,
            &worker_token,
            applied_ids.last_applied_id(),
        ) {
            Ok(request) => request,
            Err(err) => {
                report_fatal_control_loss(
                    &interviewer,
                    &cancel_token,
                    &mut first_tx,
                    &mut fatal_tx,
                    format!("failed to build worker control stream request: {err:#}"),
                )
                .await;
                return;
            }
        };

        match connect_worker_control_stream(request).await {
            Ok(mut socket) => {
                if let Some(first_tx) = first_tx.take() {
                    let _ = first_tx.send(Ok(()));
                }
                backoff = WORKER_CONTROL_RECONNECT_INITIAL_BACKOFF;
                match handle_worker_control_socket(
                    &mut socket,
                    &interviewer,
                    &cancel_token,
                    &controls,
                    &mut applied_ids,
                    &done,
                )
                .await
                {
                    Ok(()) => {}
                    Err(WorkerControlConnectError::InvalidCursor) => {
                        report_fatal_control_loss(
                            &interviewer,
                            &cancel_token,
                            &mut first_tx,
                            &mut fatal_tx,
                            "worker control stream replay cursor is invalid".to_string(),
                        )
                        .await;
                        return;
                    }
                    Err(WorkerControlConnectError::Other(err)) => {
                        tracing::debug!(error = %err, "Worker control stream disconnected");
                    }
                }
            }
            Err(WorkerControlConnectError::InvalidCursor) => {
                report_fatal_control_loss(
                    &interviewer,
                    &cancel_token,
                    &mut first_tx,
                    &mut fatal_tx,
                    "worker control stream replay cursor is invalid".to_string(),
                )
                .await;
                return;
            }
            Err(WorkerControlConnectError::Other(err)) => {
                tracing::debug!(error = %err, "Worker control stream connection failed");
            }
        }

        sleep_or_done(&done, backoff).await;
        backoff = next_worker_control_reconnect_backoff(backoff);
    }
}

async fn report_fatal_control_loss(
    interviewer: &ControlInterviewer,
    cancel_token: &CancellationToken,
    first_tx: &mut Option<oneshot::Sender<Result<()>>>,
    fatal_tx: &mut Option<oneshot::Sender<anyhow::Error>>,
    detail: String,
) {
    let message = format!("worker control channel lost: {detail}");
    interviewer.interrupt_all().await;
    if let Some(first_tx) = first_tx.take() {
        let _ = first_tx.send(Err(anyhow!(message.clone())));
    }
    if let Some(fatal_tx) = fatal_tx.take() {
        let _ = fatal_tx.send(anyhow!(message));
    }
    cancel_token.cancel();
}

async fn sleep_or_done(done: &CancellationToken, delay: Duration) {
    tokio::select! {
        () = done.cancelled() => {}
        () = time::sleep(delay) => {}
    }
}

fn next_worker_control_reconnect_backoff(current: Duration) -> Duration {
    current
        .saturating_mul(2)
        .min(WORKER_CONTROL_RECONNECT_MAX_BACKOFF)
}

fn build_worker_control_stream_request(
    target: &ServerTarget,
    run_id: &RunId,
    worker_token: &str,
    after: Option<&str>,
) -> Result<WorkerControlStreamConnectRequest> {
    let (url, unix_socket_path) = match target {
        ServerTarget::HttpUrl(_) => {
            let base = target
                .as_http_url()
                .context("HTTP server target missing URL")?;
            let websocket_base = if let Some(rest) = base.strip_prefix("http://") {
                format!("ws://{rest}")
            } else if let Some(rest) = base.strip_prefix("https://") {
                format!("wss://{rest}")
            } else {
                anyhow::bail!("unsupported server URL scheme");
            };
            (
                worker_control_stream_url(&websocket_base, run_id, after),
                None,
            )
        }
        ServerTarget::UnixSocket(path) => {
            let url = worker_control_stream_url("ws://fabro", run_id, after);
            (url, Some(path.as_path().to_path_buf()))
        }
    };
    let mut request = url
        .as_str()
        .into_client_request()
        .context("failed to build worker control stream request")?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {worker_token}"))
            .context("failed to build worker control stream authorization header")?,
    );
    Ok(WorkerControlStreamConnectRequest {
        request,
        unix_socket_path,
    })
}

fn worker_control_stream_url(base: &str, run_id: &RunId, after: Option<&str>) -> String {
    let mut url = format!("{base}/api/v1/runs/{run_id}/worker/control-stream");
    if let Some(after) = after {
        url.push_str("?after=");
        url.push_str(after);
    }
    url
}

async fn connect_worker_control_stream(
    request: WorkerControlStreamConnectRequest,
) -> Result<WorkerControlSocket, WorkerControlConnectError> {
    if let Some(path) = request.unix_socket_path.as_ref() {
        #[cfg(unix)]
        {
            let ws_request = request.request_for_tungstenite();
            let stream = UnixStream::connect(path)
                .await
                .map_err(|err| WorkerControlConnectError::Other(anyhow::Error::new(err)))?;
            let (socket, _) = tokio_tungstenite::client_async(ws_request, stream)
                .await
                .map_err(classify_tungstenite_error)?;
            Ok(WorkerControlSocket::Unix(Box::new(socket)))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(WorkerControlConnectError::Other(anyhow!(
                "Unix-socket worker control stream is not supported on this platform"
            )))
        }
    } else {
        let (socket, _) = connect_async(request.request)
            .await
            .map_err(classify_tungstenite_error)?;
        Ok(WorkerControlSocket::Tcp(Box::new(socket)))
    }
}

fn classify_tungstenite_error(error: tungstenite::Error) -> WorkerControlConnectError {
    if let tungstenite::Error::Http(response) = &error {
        if response.status().as_u16() == 410 {
            return WorkerControlConnectError::InvalidCursor;
        }
    }
    WorkerControlConnectError::Other(anyhow::Error::new(error))
}

async fn handle_worker_control_socket(
    socket: &mut WorkerControlSocket,
    interviewer: &ControlInterviewer,
    cancel_token: &CancellationToken,
    controls: &WorkerControls,
    applied_ids: &mut AppliedWorkerControlDeliveryIds,
    done: &CancellationToken,
) -> Result<(), WorkerControlConnectError> {
    let mut ping_interval = time::interval(WORKER_CONTROL_WS_PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_liveness = Instant::now();
    let liveness_timeout = time::sleep_until(last_liveness + WORKER_CONTROL_WS_LIVENESS_TIMEOUT);
    tokio::pin!(liveness_timeout);

    loop {
        liveness_timeout
            .as_mut()
            .reset(last_liveness + WORKER_CONTROL_WS_LIVENESS_TIMEOUT);

        tokio::select! {
            () = done.cancelled() => return Ok(()),
            _ = ping_interval.tick() => {
                socket
                    .send(WebSocketMessage::Ping(Vec::new().into()))
                    .await
                    .map_err(|err| WorkerControlConnectError::Other(anyhow::Error::new(err)))?;
            }
            () = &mut liveness_timeout => {
                let _ = socket
                    .send(WebSocketMessage::Close(Some(protocol::CloseFrame {
                        code: CloseCode::Away,
                        reason: WORKER_CONTROL_PONG_TIMEOUT_REASON.into(),
                    })))
                    .await;
                return Err(WorkerControlConnectError::Other(anyhow!(
                    "worker control WebSocket liveness timed out"
                )));
            }
            message = socket.next() => {
                let Some(message) = message else {
                    return Ok(());
                };
                match message {
                    Ok(WebSocketMessage::Text(text)) => {
                        last_liveness = Instant::now();
                        let frame = serde_json::from_str::<WorkerControlDeliveryFrame>(text.as_str())
                            .map_err(|err| WorkerControlConnectError::Other(anyhow::Error::new(err)))?;
                        let applied = apply_worker_control_delivery_frame(
                            interviewer,
                            cancel_token,
                            controls,
                            applied_ids,
                            frame,
                        )
                        .await;
                        if let Some(ack) = applied.ack {
                            // The answer goes back over the stream the
                            // control came in on; a stream that is gone
                            // reconnects, and the server's wait runs out.
                            let text = serde_json::to_string(&ack)
                                .map_err(|err| WorkerControlConnectError::Other(anyhow::Error::new(err)))?;
                            socket
                                .send(WebSocketMessage::Text(text.into()))
                                .await
                                .map_err(|err| WorkerControlConnectError::Other(anyhow::Error::new(err)))?;
                        }
                    }
                    Ok(WebSocketMessage::Ping(payload)) => {
                        last_liveness = Instant::now();
                        socket
                            .send(WebSocketMessage::Pong(payload))
                            .await
                            .map_err(|err| WorkerControlConnectError::Other(anyhow::Error::new(err)))?;
                    }
                    Ok(WebSocketMessage::Pong(_) | WebSocketMessage::Binary(_)) => {
                        last_liveness = Instant::now();
                    }
                    Ok(WebSocketMessage::Close(frame)) => {
                        if frame.as_ref().is_some_and(|frame| {
                            frame.reason.as_str() == WORKER_CONTROL_INVALID_CURSOR_REASON
                        }) {
                            return Err(WorkerControlConnectError::InvalidCursor);
                        }
                        return Ok(());
                    }
                    Ok(WebSocketMessage::Frame(_)) => {}
                    Err(err) => {
                        return Err(WorkerControlConnectError::Other(anyhow::Error::new(err)));
                    }
                }
            }
        }
    }
}

/// What a delivery frame came to: whether it was applied (a duplicate is
/// not), and the acknowledgement to send back when the control asked for
/// one.
#[derive(Debug, Default, PartialEq, Eq)]
struct AppliedDelivery {
    applied: bool,
    ack:     Option<WorkerControlAck>,
}

async fn apply_worker_control_delivery_frame(
    interviewer: &ControlInterviewer,
    cancel_token: &CancellationToken,
    controls: &WorkerControls,
    applied_ids: &mut AppliedWorkerControlDeliveryIds,
    frame: WorkerControlDeliveryFrame,
) -> AppliedDelivery {
    // Duplicate ids cannot reach us under normal operation: the server replays
    // strictly after the last applied id. Guard against a server-side bug or
    // reconnect race by ignoring recently-applied delivery ids.
    if applied_ids.contains(&frame.id) {
        return AppliedDelivery::default();
    }
    let frame_id = frame.id;
    let ack =
        apply_worker_control_message(interviewer, cancel_token, controls, frame.envelope).await;
    applied_ids.record(frame_id);
    AppliedDelivery { applied: true, ack }
}

/// Apply the control. The acknowledgement to send back when the control
/// carries a request id and has an outcome to report.
async fn apply_worker_control_message(
    interviewer: &ControlInterviewer,
    cancel_token: &CancellationToken,
    controls: &WorkerControls,
    message: WorkerControlEnvelope,
) -> Option<WorkerControlAck> {
    let request_id = message.request_id().map(str::to_owned);
    let outcome = match message.message {
        WorkerControlMessage::InterviewAnswer { qid, answer, actor } => {
            let _ = interviewer
                .submit(&qid, AnswerSubmission::new(answer.into(), actor))
                .await;
            None
        }
        WorkerControlMessage::RunCancel => {
            cancel_token.cancel();
            interviewer.interrupt_all().await;
            None
        }
        other => controls.apply(other).await,
    };
    Some(WorkerControlAck::new(request_id?, outcome?))
}

pub(super) fn set_worker_title(run_id: &RunId, phase: WorkerTitlePhase) {
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
        WorkerTitlePhase::Running => "running",
        WorkerTitlePhase::Paused => "paused",
        WorkerTitlePhase::Succeeded => "succeeded",
        WorkerTitlePhase::Failed => "failed",
        WorkerTitlePhase::Cancelled => "cancelled",
    };
    format!("fabro {short_id} {phase}")
}

/// `SIGTERM` and `SIGINT` cancel the run, the way the server's cancel does;
/// `SIGUSR1` pauses it and `SIGUSR2` unpauses it, the way the server's pause
/// and unpause do, through the run's controls.
pub(super) fn install_signal_handlers(
    cancel_token: CancellationToken,
    controls: RunControls,
) -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate = signal(SignalKind::terminate())?;
        let terminate_cancel = cancel_token.clone();
        tokio::spawn(async move {
            while terminate.recv().await.is_some() {
                terminate_cancel.cancel();
            }
        });

        let mut interrupt = signal(SignalKind::interrupt())?;
        tokio::spawn(async move {
            while interrupt.recv().await.is_some() {
                cancel_token.cancel();
            }
        });

        let mut pause = signal(SignalKind::user_defined1())?;
        let pause_controls = controls.clone();
        tokio::spawn(async move {
            while pause.recv().await.is_some() {
                tracing::info!("SIGUSR1: pause requested; admission is held");
                pause_controls.pause();
            }
        });

        let mut unpause = signal(SignalKind::user_defined2())?;
        tokio::spawn(async move {
            while unpause.recv().await.is_some() {
                controls.unpause().await;
                tracing::info!("SIGUSR2: unpause recorded; admission is released");
            }
        });
    }
    #[cfg(not(unix))]
    {
        let _ = (cancel_token, controls);
    }

    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_client::ServerTarget;
    use fabro_config::Storage;
    use fabro_interview::{
        AnswerValue, ControlInterviewer, Interviewer, Question, WorkerControlAck,
        WorkerControlEnvelope, WorkerControlOutcome,
    };
    use fabro_petri::test_support::MemoryPlatformRecords;
    use fabro_store::PlatformRecord;
    use fabro_types::{Principal, QuestionType, SystemActorKind, fixtures};
    use fabro_vault::{SecretType, Vault};
    use tokio::time;
    use tokio_tungstenite::tungstenite::protocol::{Message as TestWebSocketMessage, Role};
    use tokio_util::sync::CancellationToken;

    use super::super::petri_worker::PetriControls;
    use super::{
        AppliedWorkerControlDeliveryIds, RunControls, WorkerControlConnectError,
        WorkerControlSocket, WorkerControls, WorkerTitlePhase, apply_worker_control_delivery_frame,
        apply_worker_control_message, build_worker_control_stream_request,
        connect_worker_control_stream, handle_worker_control_socket, initial_worker_title_phase,
        load_worker_vault, next_worker_control_reconnect_backoff, worker_title,
    };
    use crate::args::RunWorkerMode;

    /// A run's controls over records kept in memory: what the channel
    /// tests drive.
    fn test_controls() -> WorkerControls {
        test_controls_over(Arc::new(MemoryPlatformRecords::new()))
    }

    fn test_controls_over(records: Arc<MemoryPlatformRecords>) -> WorkerControls {
        Arc::new(PetriControls::new(
            fixtures::RUN_1,
            RunControls::new(),
            records,
        ))
    }

    /// The `run.notice` records of the test run, as `(code, message)`.
    fn notices(records: &MemoryPlatformRecords) -> Vec<(String, String)> {
        records
            .records(&fixtures::RUN_1)
            .into_iter()
            .filter_map(|stored| match stored.record {
                PlatformRecord::RunNotice(notice) => Some((notice.code, notice.message)),
                _ => None,
            })
            .collect()
    }

    fn engine_actor() -> Principal {
        Principal::System {
            system_kind: SystemActorKind::Engine,
        }
    }

    #[test]
    fn clone_sandbox_credentials_are_required_for_clone_based_providers() {
        use fabro_types::SandboxProviderKind;
        assert!(SandboxProviderKind::DOCKER.clones_workspace());
        assert!(SandboxProviderKind::DAYTONA.clones_workspace());
        assert!(!SandboxProviderKind::LOCAL.clones_workspace());
    }

    #[test]
    fn fabro_run_tools_enabled_token_requires_run_tools_scope() {
        assert!(!super::fabro_run_tools_enabled_from_worker_token(
            "not-a-jwt"
        ));
        assert!(!super::fabro_run_tools_enabled_from_worker_token(
            &worker_token_with_claims(&serde_json::json!({ "scope": "run:worker" })),
        ));
        assert!(!super::fabro_run_tools_enabled_from_worker_token(
            &worker_token_with_claims(&serde_json::json!({ "scope": "agent:run_tools" })),
        ));
        assert!(!super::fabro_run_tools_enabled_from_worker_token(
            &worker_token_with_claims(&serde_json::json!({ "scope": "run:worker agent:wrong" })),
        ));
        assert!(!super::fabro_run_tools_enabled_from_worker_token(
            &worker_token_with_claims(
                &serde_json::json!({ "other": "run:worker agent:run_tools" }),
            ),
        ));
        assert!(super::fabro_run_tools_enabled_from_worker_token(
            &worker_token_with_claims(
                &serde_json::json!({ "scope": "run:worker agent:run_tools" }),
            ),
        ));
    }

    fn worker_token_with_claims(claims: &serde_json::Value) -> String {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            claims,
            &jsonwebtoken::EncodingKey::from_secret(b"test-worker-token"),
        )
        .expect("test worker token should encode")
    }

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

    #[tokio::test]
    async fn worker_control_routes_answer_by_question_id() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let mut question = Question::new("Approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let ask_interviewer = Arc::clone(&interviewer);
        let answer_task = tokio::spawn(async move { ask_interviewer.ask(question).await });

        let controls = test_controls();
        apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::interview_answer(
                "q-1",
                fabro_interview::AnswerSubmission::system(
                    fabro_interview::Answer::yes(),
                    fabro_types::SystemActorKind::Engine,
                ),
            ),
        )
        .await;

        let answer = answer_task.await.unwrap().answer;
        assert_eq!(answer.value, AnswerValue::Yes);
        assert!(!cancel_token.is_cancelled());
    }

    #[tokio::test]
    async fn worker_control_cancel_sets_cancel_token_and_interrupts_pending_interviews() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let mut question = Question::new("Approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let ask_interviewer = Arc::clone(&interviewer);
        let answer_task = tokio::spawn(async move { ask_interviewer.ask(question).await });
        tokio::task::yield_now().await;

        let controls = test_controls();
        apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::cancel_run(),
        )
        .await;

        let answer = answer_task.await.unwrap().answer;
        assert_eq!(answer.value, AnswerValue::Interrupted);
        assert!(cancel_token.is_cancelled());
    }

    #[tokio::test]
    async fn worker_control_pause_and_unpause_route_to_the_run_controls() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let controls = test_controls();

        apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::pause_run(),
        )
        .await;
        assert!(controls.controls().is_paused());

        apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::unpause_run(),
        )
        .await;
        assert!(!controls.controls().is_paused());
    }

    #[tokio::test]
    async fn a_refused_steer_is_acknowledged_with_the_notice_code() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let records = Arc::new(MemoryPlatformRecords::new());
        let controls = test_controls_over(Arc::clone(&records));

        // No live agent: refused under the control's own code.
        let ack = apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::steer("hurry up", None, engine_actor()).with_request_id("req-1"),
        )
        .await;
        assert_eq!(
            ack,
            Some(WorkerControlAck::new(
                "req-1",
                WorkerControlOutcome::Refused {
                    code:    "steer_refused".to_string(),
                    message: "Steer refused: Run has no active steerable agent session."
                        .to_string(),
                }
            ))
        );

        // A stage that is not running: `no_such_stage`, for a steer as for
        // an interrupt.
        let ack = apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::steer("hurry up", Some("work".to_string()), engine_actor())
                .with_request_id("req-2"),
        )
        .await;
        assert_eq!(
            ack,
            Some(WorkerControlAck::new(
                "req-2",
                WorkerControlOutcome::Refused {
                    code:    "no_such_stage".to_string(),
                    message: "Steer of stage `work` refused: no stage named `work` is running"
                        .to_string(),
                }
            ))
        );
        let ack = apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::interrupt(Some("work".to_string()), engine_actor())
                .with_request_id("req-3"),
        )
        .await;
        assert_eq!(
            ack,
            Some(WorkerControlAck::new(
                "req-3",
                WorkerControlOutcome::Refused {
                    code:    "no_such_stage".to_string(),
                    message: "Interrupt of stage `work` refused: no stage named `work` is running"
                        .to_string(),
                }
            ))
        );

        // Each refusal is also a notice on the run, under the same code.
        assert_eq!(
            notices(&records)
                .iter()
                .map(|(code, _)| code.as_str())
                .collect::<Vec<_>>(),
            ["steer_refused", "no_such_stage", "no_such_stage"]
        );
    }

    #[tokio::test]
    async fn a_control_without_a_request_id_is_not_acknowledged() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let records = Arc::new(MemoryPlatformRecords::new());
        let controls = test_controls_over(Arc::clone(&records));

        let ack = apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::steer("hurry up", None, engine_actor()),
        )
        .await;
        assert_eq!(ack, None);
        assert_eq!(notices(&records).len(), 1, "the refusal is still a notice");

        let ack = apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::pause_run(),
        )
        .await;
        assert_eq!(ack, None);
    }

    #[tokio::test]
    async fn muted_acknowledgements_apply_the_control_and_answer_nothing() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let records = Arc::new(MemoryPlatformRecords::new());
        let controls: WorkerControls = Arc::new(
            PetriControls::new(fixtures::RUN_1, RunControls::new(), records.clone())
                .with_muted_acks(true),
        );

        let ack = apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &controls,
            WorkerControlEnvelope::interrupt(None, engine_actor()).with_request_id("req-1"),
        )
        .await;
        assert_eq!(ack, None);
        assert_eq!(notices(&records), [(
            "interrupt_refused".to_string(),
            "Interrupt refused: Run has no active steerable agent session.".to_string()
        )]);
    }

    #[tokio::test]
    async fn duplicate_delivery_ids_are_not_applied_twice() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let controls = test_controls();
        let mut applied_ids = AppliedWorkerControlDeliveryIds::default();
        let frame = fabro_interview::WorkerControlDeliveryFrame {
            id:       "local:1".to_string(),
            envelope: WorkerControlEnvelope::pause_run(),
        };

        assert!(
            apply_worker_control_delivery_frame(
                &interviewer,
                &cancel_token,
                &controls,
                &mut applied_ids,
                frame.clone(),
            )
            .await
            .applied
        );
        assert!(
            !apply_worker_control_delivery_frame(
                &interviewer,
                &cancel_token,
                &controls,
                &mut applied_ids,
                frame,
            )
            .await
            .applied
        );

        assert_eq!(applied_ids.last_applied_id(), Some("local:1"));
    }

    #[test]
    fn worker_control_request_construction_for_http_targets() {
        let run_id = fixtures::RUN_1;
        let request = build_worker_control_stream_request(
            &ServerTarget::http_url("http://example.com:3000").unwrap(),
            &run_id,
            "worker-token",
            None,
        )
        .unwrap();
        assert_eq!(
            request.uri(),
            format!("ws://example.com:3000/api/v1/runs/{run_id}/worker/control-stream")
        );
        assert_eq!(request.authorization(), Some("Bearer worker-token"));

        let reconnect = build_worker_control_stream_request(
            &ServerTarget::http_url("https://example.com").unwrap(),
            &run_id,
            "worker-token",
            Some("local:42"),
        )
        .unwrap();
        assert_eq!(
            reconnect.uri(),
            format!("wss://example.com/api/v1/runs/{run_id}/worker/control-stream?after=local:42")
        );
    }

    #[cfg(unix)]
    #[test]
    fn worker_control_request_construction_for_unix_socket_targets() {
        let run_id = fixtures::RUN_1;
        let request = build_worker_control_stream_request(
            &ServerTarget::unix_socket_path("/tmp/fabro.sock").unwrap(),
            &run_id,
            "worker-token",
            Some("local:42"),
        )
        .unwrap();

        assert_eq!(
            request.uri(),
            format!("ws://fabro/api/v1/runs/{run_id}/worker/control-stream?after=local:42")
        );
        assert_eq!(
            request.unix_socket_path.as_deref(),
            Some(std::path::Path::new("/tmp/fabro.sock"))
        );
    }

    #[test]
    fn worker_control_reconnect_backoff_is_bounded() {
        assert_eq!(
            next_worker_control_reconnect_backoff(Duration::from_millis(100)),
            Duration::from_millis(200)
        );
        assert_eq!(
            next_worker_control_reconnect_backoff(Duration::from_secs(4)),
            Duration::from_secs(5)
        );
        assert_eq!(
            next_worker_control_reconnect_backoff(Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn worker_control_socket_times_out_without_liveness() {
        let (worker_io, _server_io) = tokio::io::duplex(1024);
        let worker_ws =
            tokio_tungstenite::WebSocketStream::from_raw_socket(worker_io, Role::Client, None)
                .await;
        let mut socket = WorkerControlSocket::Test(Box::new(worker_ws));
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let controls = test_controls();
        let mut applied_ids = AppliedWorkerControlDeliveryIds::default();
        let done = CancellationToken::new();

        let task = tokio::spawn(async move {
            handle_worker_control_socket(
                &mut socket,
                &interviewer,
                &cancel_token,
                &controls,
                &mut applied_ids,
                &done,
            )
            .await
        });

        tokio::task::yield_now().await;
        time::advance(Duration::from_secs(46)).await;
        let result = task.await.unwrap();
        assert!(matches!(result, Err(WorkerControlConnectError::Other(_))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worker_control_unix_socket_handshake_completes() {
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("fabro.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(message) = futures::StreamExt::next(&mut socket).await {
                match message.unwrap() {
                    TestWebSocketMessage::Close(_) => break,
                    TestWebSocketMessage::Ping(payload) => {
                        futures::SinkExt::send(&mut socket, TestWebSocketMessage::Pong(payload))
                            .await
                            .unwrap();
                    }
                    _ => {}
                }
            }
        });
        let request = build_worker_control_stream_request(
            &ServerTarget::unix_socket_path(&socket_path).unwrap(),
            &fixtures::RUN_1,
            "worker-token",
            None,
        )
        .unwrap();

        let mut socket = connect_worker_control_stream(request).await.unwrap();
        socket
            .send(TestWebSocketMessage::Close(None))
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn load_worker_vault_reads_credentials_from_storage_dir() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::new(temp.path());
        let mut vault = Vault::load(storage.secrets_path()).unwrap();
        vault
            .set("ANTHROPIC_API_KEY", "vault-key", SecretType::Token, None)
            .unwrap();

        let loaded = load_worker_vault(temp.path()).await.unwrap();
        let guard = loaded.read().await;
        let credential = guard.get("ANTHROPIC_API_KEY").unwrap();

        assert!(credential.contains("vault-key"));
    }
}
