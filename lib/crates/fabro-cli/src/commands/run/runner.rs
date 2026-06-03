use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use fabro_api::types::RunManifest;
use fabro_client::ServerTarget;
use fabro_config::user::active_settings_path;
use fabro_config::{ServerSettingsBuilder, Storage, load_llm_catalog_settings};
use fabro_interview::{
    AnswerSubmission, ControlInterviewer, WORKER_CONTROL_INVALID_CURSOR_REASON,
    WORKER_CONTROL_PONG_TIMEOUT_REASON, WORKER_CONTROL_WS_LIVENESS_TIMEOUT,
    WORKER_CONTROL_WS_PING_INTERVAL, WorkerControlDeliveryFrame, WorkerControlEnvelope,
    WorkerControlMessage,
};
use fabro_model::Catalog;
use fabro_server::run_tool_manifest;
use fabro_store::{EventEnvelope, RunProjection, RunProjectionReducer};
use fabro_tool::fabro_client::ClientBackend;
use fabro_types::settings::InterpString;
use fabro_types::settings::run::{RunMode, RunNamespace};
use fabro_types::{
    ArtifactUpload, EventBody, FailureReason, Principal, RunBlobId, RunEvent, RunId,
    WorkflowSettings,
};
use fabro_vault::Vault;
use fabro_workflow::artifact_upload::{ArtifactSink, StageArtifactUploader};
use fabro_workflow::event::{Emitter, RunEventSink};
use fabro_workflow::operations::{self, StartServices};
use fabro_workflow::run_control::RunControlState;
use fabro_workflow::runtime_store::{RunStoreBackend, RunStoreHandle};
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
use tokio::sync::{Mutex, RwLock as AsyncRwLock, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Request as WebSocketRequest, header};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{self, Message as WebSocketMessage};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite};
use tokio_util::sync::CancellationToken;

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
    run_dir: PathBuf,
    mode: RunWorkerMode,
    worker_token: &str,
) -> Result<()> {
    let _ = fabro_proc::title_init();
    set_worker_title(&run_id, initial_worker_title_phase(mode));

    let target = server.parse::<ServerTarget>()?;
    let client = server_client::connect_server_target_with_bearer(&target, worker_token).await?;
    let run_store = HttpRunStore::connect(run_id, client.clone_for_reuse()).await?;
    let run_state = run_store
        .state()
        .await
        .with_context(|| format!("failed to load run state for {run_id}"))?;
    let run_spec = &run_state.spec;
    let llm_catalog_settings =
        load_llm_catalog_settings(None).context("failed to load worker LLM catalog settings")?;
    let catalog = Arc::new(
        Catalog::from_builtin_with_overrides(&llm_catalog_settings)
            .context("failed to build worker LLM catalog")?,
    );
    let artifact_sink = Some(ArtifactSink::Uploader(build_artifact_uploader(
        run_id,
        client.clone_for_reuse(),
        worker_token.to_owned(),
    )));
    let fabro_run_tools = if fabro_run_tools_enabled_from_worker_token(worker_token) {
        build_fabro_run_tool_services(
            worker_token,
            client.clone_for_reuse(),
            run_id,
            run_spec.source_directory.as_deref(),
            &run_dir,
            Arc::clone(&catalog),
        )
    } else {
        None
    };
    let interviewer = Arc::new(ControlInterviewer::new());
    let cancel_token = CancellationToken::new();
    let emitter = Arc::new(Emitter::new(run_id));
    let steering_hub = Arc::new(fabro_workflow::SteeringHub::new(Arc::clone(&emitter)));
    let run_control = RunControlState::new();
    install_signal_handlers(Arc::clone(&run_control), cancel_token.clone())?;
    let mut control_manager = if run_state.status.is_terminal() {
        None
    } else {
        Some(spawn_worker_control_manager(
            target.clone(),
            run_id,
            worker_token.to_owned(),
            Arc::clone(&interviewer),
            cancel_token.clone(),
            Arc::clone(&steering_hub),
            Arc::clone(&run_control),
        ))
    };
    if let Some(control_manager) = &mut control_manager {
        control_manager.wait_for_first_connection().await?;
    }
    let vault = load_worker_vault(storage_dir.as_deref())?;
    let github_app = {
        let vault_guard = match &vault {
            Some(arc) => Some(arc.read().await),
            None => None,
        };
        maybe_build_github_credentials(&run_spec.settings, vault_guard.as_deref())?
    };
    let services = StartServices {
        run_id,
        cancel_token: cancel_token.clone(),
        emitter,
        interviewer,
        steering_hub,
        run_store: run_store.clone(),
        event_sink: RunEventSink::map(
            stamp_system_worker,
            RunEventSink::fanout(vec![
                RunEventSink::backend(run_store),
                RunEventSink::callback(move |event| {
                    update_worker_title_from_event(&event);
                    async move { Ok(()) }
                }),
            ]),
        ),
        artifact_sink,
        run_control: Some(run_control),
        github_app,
        github_permissions: run_spec
            .settings
            .run
            .integrations
            .github
            .resolve_permissions(process_env_var),
        vault,
        catalog,
        on_node: None,
        registry_override: None,
        fabro_run_tools,
    };

    let execution = async {
        match mode {
            RunWorkerMode::Start => operations::start(&run_dir, services).await,
            RunWorkerMode::Resume => operations::resume(&run_dir, services).await,
        }
    };

    if let Some(mut control_manager) = control_manager {
        tokio::select! {
            result = execution => {
                control_manager.finish();
                result?;
            }
            fatal = control_manager.fatal_control_loss() => {
                control_manager.finish();
                return Err(fatal);
            }
        }
    } else {
        execution.await?;
    }

    Ok(())
}

const WORKER_TOKEN_SCOPE: &str = "run:worker";
const WORKER_RUN_TOOLS_SCOPE: &str = "agent:run_tools";

#[derive(serde::Deserialize)]
struct WorkerTokenScopeClaim {
    scope: String,
}

fn fabro_run_tools_enabled_from_worker_token(worker_token: &str) -> bool {
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

fn build_fabro_run_tool_services(
    worker_token: &str,
    client: fabro_client::Client,
    current_run_id: RunId,
    source_directory: Option<&str>,
    run_dir: &Path,
    catalog: Arc<Catalog>,
) -> Option<FabroRunToolServices> {
    if worker_token.trim().is_empty() {
        return None;
    }
    let backend = ClientBackend::new(Arc::new(client))
        .with_manifest_builder(Arc::new(WorkerRunManifestBuilder { catalog }));
    Some(FabroRunToolServices {
        backend: Arc::new(backend),
        current_run_id,
        base_cwd: source_directory.map_or_else(|| run_dir.to_path_buf(), PathBuf::from),
        user_settings_path: active_settings_path(None),
    })
}

struct WorkerRunManifestBuilder {
    catalog: Arc<Catalog>,
}

impl fabro_tool::RunManifestBuilder for WorkerRunManifestBuilder {
    fn build_run_manifest(
        &self,
        spec: &fabro_tool::ValidatedCreateRunSpec,
        cwd: &Path,
        user_settings_path: &Path,
    ) -> fabro_tool::ToolResult<RunManifest> {
        run_tool_manifest::build_run_tool_manifest(
            spec,
            cwd,
            user_settings_path,
            Arc::clone(&self.catalog),
        )
    }
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

struct WorkerControlManagerHandle {
    first_connection: Option<oneshot::Receiver<Result<()>>>,
    fatal:            Option<oneshot::Receiver<anyhow::Error>>,
    done:             CancellationToken,
    task:             JoinHandle<()>,
}

impl WorkerControlManagerHandle {
    async fn wait_for_first_connection(&mut self) -> Result<()> {
        let receiver = self
            .first_connection
            .take()
            .context("worker control first-connection receiver missing")?;
        receiver
            .await
            .context("worker control manager stopped before first connection")?
    }

    async fn fatal_control_loss(&mut self) -> anyhow::Error {
        let Some(receiver) = self.fatal.take() else {
            return anyhow!("worker control fatal receiver missing");
        };
        receiver
            .await
            .unwrap_or_else(|_| anyhow!("worker control manager stopped before workflow completed"))
    }

    fn finish(&self) {
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

fn spawn_worker_control_manager(
    target: ServerTarget,
    run_id: RunId,
    worker_token: String,
    interviewer: Arc<ControlInterviewer>,
    cancel_token: CancellationToken,
    steering_hub: Arc<fabro_workflow::SteeringHub>,
    run_control: Arc<RunControlState>,
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
            steering_hub,
            run_control,
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
    steering_hub: Arc<fabro_workflow::SteeringHub>,
    run_control: Arc<RunControlState>,
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
                    &steering_hub,
                    &run_control,
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
    steering_hub: &fabro_workflow::SteeringHub,
    run_control: &RunControlState,
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
                        apply_worker_control_delivery_frame(
                            interviewer,
                            cancel_token,
                            steering_hub,
                            run_control,
                            applied_ids,
                            frame,
                        )
                        .await;
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

async fn apply_worker_control_delivery_frame(
    interviewer: &ControlInterviewer,
    cancel_token: &CancellationToken,
    steering_hub: &fabro_workflow::SteeringHub,
    run_control: &RunControlState,
    applied_ids: &mut AppliedWorkerControlDeliveryIds,
    frame: WorkerControlDeliveryFrame,
) -> bool {
    // Duplicate ids cannot reach us under normal operation: the server replays
    // strictly after the last applied id. Guard against a server-side bug or
    // reconnect race by ignoring recently-applied delivery ids.
    if applied_ids.contains(&frame.id) {
        return false;
    }
    let frame_id = frame.id;
    apply_worker_control_message(
        interviewer,
        cancel_token,
        steering_hub,
        run_control,
        frame.envelope,
    )
    .await;
    applied_ids.record(frame_id);
    true
}

async fn apply_worker_control_message(
    interviewer: &ControlInterviewer,
    cancel_token: &CancellationToken,
    steering_hub: &fabro_workflow::SteeringHub,
    run_control: &RunControlState,
    message: WorkerControlEnvelope,
) {
    match message.message {
        WorkerControlMessage::InterviewAnswer { qid, answer, actor } => {
            let _ = interviewer
                .submit(&qid, AnswerSubmission::new(answer.into(), actor))
                .await;
        }
        WorkerControlMessage::RunCancel => {
            cancel_token.cancel();
            interviewer.interrupt_all().await;
        }
        WorkerControlMessage::RunPause => {
            run_control.request_pause();
        }
        WorkerControlMessage::RunUnpause => {
            run_control.request_unpause();
        }
        WorkerControlMessage::Steer { text, actor } => {
            steering_hub.deliver_steer(text, Some(actor));
        }
        WorkerControlMessage::Interrupt { actor } => {
            steering_hub.interrupt(Some(&actor));
        }
        WorkerControlMessage::InterruptThenSteer { text, actor } => {
            steering_hub.interrupt_then_steer(&text, Some(&actor));
        }
        WorkerControlMessage::PairStart {
            run_id,
            pair_id,
            target,
            actor,
        } => {
            let _ = steering_hub.start_pair(run_id, pair_id, target, Some(actor));
        }
        WorkerControlMessage::PairMessage {
            pair_id,
            message_id,
            text,
            client_message_id,
            actor,
        } => {
            let _ = steering_hub.send_pair_message(
                pair_id,
                message_id,
                text,
                client_message_id,
                Some(actor),
            );
        }
        WorkerControlMessage::PairEnd { pair_id, actor } => {
            let _ = steering_hub.end_pair(pair_id, Some(actor));
        }
    }
}

fn build_artifact_uploader(
    run_id: RunId,
    client: server_client::Client,
    worker_token: String,
) -> Arc<dyn StageArtifactUploader> {
    Arc::new(HttpArtifactUploader {
        run_id,
        client,
        worker_token,
    })
}

struct HttpArtifactUploader {
    run_id:       RunId,
    client:       server_client::Client,
    worker_token: String,
}

#[async_trait]
impl StageArtifactUploader for HttpArtifactUploader {
    async fn upload_stage_artifacts(
        &self,
        stage_id: &fabro_types::StageId,
        retry: u32,
        artifact_capture_dir: &Path,
        artifacts: &[ArtifactUpload],
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
                    retry,
                    &artifact.path,
                    &artifact_capture_dir.join(&artifact.path),
                    &self.worker_token,
                )
                .await;
        }

        self.client
            .upload_stage_artifact_batch(
                &self.run_id,
                stage_id,
                retry,
                artifact_capture_dir,
                artifacts,
                &self.worker_token,
            )
            .await
    }
}

#[derive(Clone)]
struct HttpRunStore {
    run_id: RunId,
    client: server_client::Client,
    state:  Arc<Mutex<RunProjection>>,
    events: Arc<Mutex<Option<Vec<EventEnvelope>>>>,
}

impl HttpRunStore {
    async fn connect(run_id: RunId, client: server_client::Client) -> Result<RunStoreHandle> {
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
                time::sleep(*delay).await;
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
        let envelope = EventEnvelope {
            seq,
            event: event.clone(),
        };

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
        let seq = Box::pin(self.with_retries("append run event", || {
            let client = self.client.clone_for_reuse();
            let run_id = self.run_id;
            let event = event.clone();
            async move { client.append_run_event(&run_id, &event).await }
        }))
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

    async fn read_run_log(&self) -> Result<Option<Vec<u8>>> {
        self.with_retries("get run logs", || {
            let client = self.client.clone_for_reuse();
            let run_id = self.run_id;
            async move { client.get_run_logs(&run_id).await }
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
        EventBody::RunFailed(props) => Some(if props.failure.reason == FailureReason::Cancelled {
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

fn stamp_system_worker(mut event: RunEvent) -> RunEvent {
    if event.actor.is_none() {
        event.actor = Some(Principal::Worker {
            run_id: event.run_id,
        });
    }
    event
}

#[expect(
    clippy::disallowed_methods,
    reason = "known leak: GitHub App id/slug passes unresolved; strict resolution scheduled in the \
              interpolation unification (Phase 2)"
)]
fn maybe_build_github_credentials(
    settings: &WorkflowSettings,
    vault: Option<&fabro_vault::Vault>,
) -> Result<Option<fabro_github::GitHubCredentials>> {
    let resolved_run = &settings.run;
    let resolved_server = ServerSettingsBuilder::load_default().ok();
    let server_ns = resolved_server.as_ref().map(|s| &s.server);
    let strategy = server_ns
        .map(|server| server.integrations.github.strategy)
        .unwrap_or_default();
    let app_id = server_ns
        .and_then(|server| server.integrations.github.app_id.as_ref())
        .map(InterpString::as_source);
    let app_slug = server_ns
        .and_then(|server| server.integrations.github.slug.as_ref())
        .map(InterpString::as_source);

    if requires_github_credentials(resolved_run) {
        return build_github_credentials(strategy, app_id.as_deref(), app_slug.as_deref(), vault);
    }

    let pull_request_enabled =
        resolved_run.execution.mode != RunMode::DryRun && resolved_run.pull_request.is_some();
    if pull_request_enabled {
        return Ok(build_github_credentials(
            strategy,
            app_id.as_deref(),
            app_slug.as_deref(),
            vault,
        )
        .ok()
        .flatten());
    }

    Ok(None)
}

#[expect(
    clippy::disallowed_methods,
    reason = "CLI worker InterpString resolution facade for {{ env.* }} values."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Hard-gate for the CLI worker path: a run-level token is requested, or
/// a clone-based sandbox in non-dry-run mode will need credentials to
/// pull the repository. Pull-request-driven credential acquisition is
/// handled separately by the caller as a soft fallback.
fn requires_github_credentials(run: &RunNamespace) -> bool {
    if run.integrations.github.is_token_requested() {
        return true;
    }
    run.execution.mode != RunMode::DryRun && run.environment.provider.is_clone_based()
}

fn install_signal_handlers(
    run_control: Arc<RunControlState>,
    cancel_token: CancellationToken,
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

    use chrono::Utc;
    use fabro_client::ServerTarget;
    use fabro_config::Storage;
    use fabro_interview::{
        AnswerValue, ControlInterviewer, Interviewer, Question, WorkerControlEnvelope,
    };
    use fabro_types::run_event::{
        InterviewCompletedProps, InterviewStartedProps, RunCompletedProps, RunControlEffectProps,
        RunFailedProps, RunStatusTransitionProps,
    };
    use fabro_types::{
        AuthMethod, EventBody, FailureCategory, FailureDetail, FailureReason, IdpIdentity,
        Principal, QuestionType, RunFailure, SuccessReason, fixtures,
    };
    use fabro_vault::{SecretType, Vault};
    use fabro_workflow::event::RunEventSink;
    use fabro_workflow::run_control::RunControlState;
    use tokio::time;
    use tokio_tungstenite::tungstenite::protocol::{Message as TestWebSocketMessage, Role};
    use tokio_util::sync::CancellationToken;

    use super::{
        AppliedWorkerControlDeliveryIds, WorkerControlConnectError, WorkerControlSocket,
        WorkerTitlePhase, apply_worker_control_delivery_frame, apply_worker_control_message,
        build_worker_control_stream_request, connect_worker_control_stream,
        handle_worker_control_socket, initial_worker_title_phase, load_worker_vault,
        next_worker_control_reconnect_backoff, stamp_system_worker, worker_title,
        worker_title_phase_for_event,
    };
    use crate::args::RunWorkerMode;

    fn test_steering_hub() -> Arc<fabro_workflow::SteeringHub> {
        let emitter = Arc::new(fabro_workflow::event::Emitter::new(fixtures::RUN_1));
        Arc::new(fabro_workflow::SteeringHub::new(emitter))
    }

    #[test]
    fn clone_sandbox_credentials_are_required_for_clone_based_providers() {
        use fabro_types::settings::run::EnvironmentProvider;
        assert!(EnvironmentProvider::Docker.is_clone_based());
        assert!(EnvironmentProvider::Daytona.is_clone_based());
        assert!(!EnvironmentProvider::Local.is_clone_based());
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

    fn test_user_principal(login: &str) -> Principal {
        Principal::user(
            IdpIdentity::new("https://github.com", "12345").unwrap(),
            login.to_string(),
            AuthMethod::Github,
        )
    }

    fn running_event(actor: Option<Principal>) -> fabro_types::RunEvent {
        fabro_types::RunEvent {
            id: "evt_1".to_string(),
            ts: Utc::now(),
            run_id: fixtures::RUN_1,
            node_id: None,
            node_label: None,
            stage_id: None,
            parallel_group_id: None,
            parallel_branch_id: None,
            session_id: None,
            parent_session_id: None,
            tool_call_id: None,
            actor,
            body: EventBody::RunRunning(RunStatusTransitionProps::default()),
        }
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

    #[test]
    fn worker_title_phase_tracks_lifecycle_events() {
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunStarting(RunStatusTransitionProps {})),
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
                timing:               fabro_types::RunTiming::wall_only(10),
                artifact_count:       0,
                status:               "succeeded".to_string(),
                reason:               SuccessReason::Completed,
                total_usd_micros:     None,
                final_git_commit_sha: None,
                final_patch:          None,
                diff_summary:         None,
                billing:              None,
            })),
            Some(WorkerTitlePhase::Succeeded)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunFailed(RunFailedProps {
                failure:              RunFailure {
                    reason: FailureReason::Cancelled,
                    detail: FailureDetail::new("cancelled", FailureCategory::Canceled),
                },
                timing:               fabro_types::RunTiming::wall_only(10),
                final_git_commit_sha: None,
                final_patch:          None,
                diff_summary:         None,
                billing:              None,
            })),
            Some(WorkerTitlePhase::Cancelled)
        );
        assert_eq!(
            worker_title_phase_for_event(&EventBody::RunFailed(RunFailedProps {
                failure:              RunFailure {
                    reason: FailureReason::Terminated,
                    detail: FailureDetail::new("boom", FailureCategory::Deterministic),
                },
                timing:               fabro_types::RunTiming::wall_only(10),
                final_git_commit_sha: None,
                final_patch:          None,
                diff_summary:         None,
                billing:              None,
            })),
            Some(WorkerTitlePhase::Failed)
        );
    }

    #[test]
    fn stamp_system_worker_fills_missing_actor_only() {
        let stamped = stamp_system_worker(running_event(None));

        assert_eq!(
            stamped.actor,
            Some(Principal::Worker {
                run_id: fixtures::RUN_1,
            })
        );

        let existing_actor = test_user_principal("octocat");
        let stamped = stamp_system_worker(running_event(Some(existing_actor.clone())));
        assert_eq!(stamped.actor, Some(existing_actor));
    }

    #[tokio::test]
    async fn worker_event_stamp_applies_to_all_fanout_sinks() {
        let first = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let second = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let first_events = Arc::clone(&first);
        let second_events = Arc::clone(&second);
        let sink = RunEventSink::map(
            stamp_system_worker,
            RunEventSink::fanout(vec![
                RunEventSink::callback(move |event| {
                    let first_events = Arc::clone(&first_events);
                    async move {
                        first_events.lock().await.push(event);
                        Ok(())
                    }
                }),
                RunEventSink::callback(move |event| {
                    let second_events = Arc::clone(&second_events);
                    async move {
                        second_events.lock().await.push(event);
                        Ok(())
                    }
                }),
            ]),
        );
        let event = running_event(None);

        sink.write_run_event(&event).await.unwrap();

        let first = first.lock().await;
        let second = second.lock().await;
        assert_eq!(
            first[0].actor,
            Some(Principal::Worker {
                run_id: fixtures::RUN_1,
            })
        );
        assert_eq!(
            second[0].actor,
            Some(Principal::Worker {
                run_id: fixtures::RUN_1,
            })
        );
    }

    #[tokio::test]
    async fn worker_control_routes_answer_by_question_id() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let run_control = RunControlState::new();
        let mut question = Question::new("Approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let ask_interviewer = Arc::clone(&interviewer);
        let answer_task = tokio::spawn(async move { ask_interviewer.ask(question).await });

        let hub = test_steering_hub();
        apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &hub,
            &run_control,
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
        let run_control = RunControlState::new();
        let mut question = Question::new("Approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let ask_interviewer = Arc::clone(&interviewer);
        let answer_task = tokio::spawn(async move { ask_interviewer.ask(question).await });
        tokio::task::yield_now().await;

        let hub = test_steering_hub();
        apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &hub,
            &run_control,
            WorkerControlEnvelope::cancel_run(),
        )
        .await;

        let answer = answer_task.await.unwrap().answer;
        assert_eq!(answer.value, AnswerValue::Interrupted);
        assert!(cancel_token.is_cancelled());
    }

    #[tokio::test]
    async fn worker_control_pause_and_unpause_route_to_run_control() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let run_control = RunControlState::new();
        let hub = test_steering_hub();

        apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &hub,
            &run_control,
            WorkerControlEnvelope::pause_run(),
        )
        .await;
        assert!(run_control.pause_requested());

        apply_worker_control_message(
            &interviewer,
            &cancel_token,
            &hub,
            &run_control,
            WorkerControlEnvelope::unpause_run(),
        )
        .await;
        assert!(!run_control.pause_requested());
    }

    #[tokio::test]
    async fn duplicate_delivery_ids_are_not_applied_twice() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let cancel_token = CancellationToken::new();
        let run_control = RunControlState::new();
        let hub = test_steering_hub();
        let mut applied_ids = AppliedWorkerControlDeliveryIds::default();
        let frame = fabro_interview::WorkerControlDeliveryFrame {
            id:       "local:1".to_string(),
            envelope: WorkerControlEnvelope::pause_run(),
        };

        assert!(
            apply_worker_control_delivery_frame(
                &interviewer,
                &cancel_token,
                &hub,
                &run_control,
                &mut applied_ids,
                frame.clone(),
            )
            .await
        );
        assert!(
            !apply_worker_control_delivery_frame(
                &interviewer,
                &cancel_token,
                &hub,
                &run_control,
                &mut applied_ids,
                frame,
            )
            .await
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
        let hub = test_steering_hub();
        let run_control = RunControlState::new();
        let mut applied_ids = AppliedWorkerControlDeliveryIds::default();
        let done = CancellationToken::new();

        let task = tokio::spawn(async move {
            handle_worker_control_socket(
                &mut socket,
                &interviewer,
                &cancel_token,
                &hub,
                &run_control,
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

        let loaded = load_worker_vault(Some(temp.path())).unwrap().unwrap();
        let guard = loaded.read().await;
        let credential = guard.get("ANTHROPIC_API_KEY").unwrap();

        assert!(credential.contains("vault-key"));
    }

    mod requires_github_credentials_truth_table {
        //! Truth-table coverage for the worker-side credential gate.
        //! `InterpString` → `String` resolution is tested in `fabro-types`
        //! next to `RunIntegrationsGithubSettings::resolve_permissions`.

        use std::collections::HashMap;

        use fabro_types::settings::InterpString;
        use fabro_types::settings::run::{
            EnvironmentProvider, RunIntegrationsGithubSettings, RunIntegrationsSettings, RunMode,
            RunNamespace,
        };

        use super::super::requires_github_credentials;

        fn run_with(
            permissions: HashMap<String, InterpString>,
            provider: &str,
            mode: RunMode,
        ) -> RunNamespace {
            let mut run = RunNamespace::default();
            run.execution.mode = mode;
            run.environment.provider = provider
                .parse::<EnvironmentProvider>()
                .expect("test provider should parse");
            run.integrations = RunIntegrationsSettings {
                github: RunIntegrationsGithubSettings { permissions },
            };
            run
        }

        #[test]
        fn requires_github_credentials_when_permissions_non_empty() {
            let permissions = HashMap::from([("issues".to_string(), InterpString::parse("read"))]);
            // Even with local sandbox + dry-run, non-empty permissions
            // force credential acquisition.
            let run = run_with(permissions, "local", RunMode::DryRun);
            assert!(requires_github_credentials(&run));
        }

        #[test]
        fn requires_github_credentials_for_clone_based_provider() {
            let run = run_with(HashMap::new(), "docker", RunMode::Normal);
            assert!(requires_github_credentials(&run));

            let daytona = run_with(HashMap::new(), "daytona", RunMode::Normal);
            assert!(requires_github_credentials(&daytona));
        }

        #[test]
        fn does_not_require_github_credentials_for_local_clean_run() {
            let run = run_with(HashMap::new(), "local", RunMode::Normal);
            assert!(!requires_github_credentials(&run));
        }

        #[test]
        fn does_not_require_github_credentials_for_clone_provider_in_dry_run() {
            let run = run_with(HashMap::new(), "docker", RunMode::DryRun);
            assert!(!requires_github_credentials(&run));
        }
    }
}
