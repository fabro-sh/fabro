use std::collections::BTreeMap;
use std::io::Cursor;
use std::num::{NonZeroU16, NonZeroU64};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use fabro_redact::SecretRedactor;
use fabro_types::{RunSandboxInstance, SandboxProviderKind};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use pebble_coding_agent::sandbox_driver::{display_for_log, resolve_path};
use sandbox_driver::{
    Error as DriverError, ExecControls, ExecSpec, Expose, FileKind, OutputSink, PortSupport,
    Sandbox, TerminalSize,
};
use tokio::io::{self as tokio_io, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::super::{
    ApiError, AppState, Bytes, HeaderMap, IntoResponse, Json, Path, PreviewUrlRequest,
    PreviewUrlResponse, Query, RequiredUser, Response, Router, RunId, SandboxDetails,
    SandboxFileEntry, SandboxFileListResponse, SandboxService, SandboxServiceListResponse,
    SshAccessRequest, SshAccessResponse, State, StatusCode, VncPreviewResponse, get,
    octet_stream_response, parse_run_id_path, post, reject_if_archived,
};
use crate::sandbox_access::{self, ProviderAccess, RunSandbox};
use crate::sandbox_ports::{self, ListeningPort};

const MAX_TERMINAL_CONTROL_BYTES: usize = 4096;
const DEFAULT_VNC_NO_VNC_PORT: u16 = 6080;
const DEFAULT_VNC_TTL_SECS: i32 = 3600;
/// The terminal's size until the browser sends its own.
const DEFAULT_TERMINAL_SIZE: TerminalSize = TerminalSize {
    rows:    NonZeroU16::new(24).unwrap(),
    columns: NonZeroU16::new(80).unwrap(),
};
/// Header a Daytona unsigned preview needs; surfaced as the response token.
const PREVIEW_TOKEN_HEADER: &str = "x-daytona-preview-token";
const LIST_SANDBOX_SERVICES_FAILURE_LABEL: &str = "sandbox service discovery";
// Daytona's signed preview points at the noVNC service root, which serves a
// directory listing. Force the iframe to the actual viewer page with
// autoconnect+scale so the user lands on the desktop, not a file index.
const VNC_VIEWER_PATH: &str = "/vnc.html";
const VNC_VIEWER_AUTOCONNECT: (&str, &str) = ("autoconnect", "true");
const VNC_VIEWER_RESIZE: (&str, &str) = ("resize", "scale");

/// The provider-side steps behind a VNC preview, so the response shaping can
/// be tested without a sandbox.
trait VncSandbox {
    /// Starts the desktop and returns the signed viewer URL the provider
    /// hands out for it.
    fn vnc_viewer_url(&self) -> BoxFuture<'_, anyhow::Result<String>>;
}

impl VncSandbox for Arc<dyn Sandbox> {
    fn vnc_viewer_url(&self) -> BoxFuture<'_, anyhow::Result<String>> {
        async move {
            let endpoints = self.endpoints().ok_or_else(|| {
                anyhow::anyhow!("Sandbox provider does not support VNC previews.")
            })?;
            endpoints
                .vnc()
                .await
                .map(|connection| connection.url)
                .context("Failed to open a VNC preview")
        }
        .boxed()
    }
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/preview", post(generate_preview_url))
        .route("/runs/{id}/ssh", post(create_ssh_access))
        .route("/runs/{id}/terminal", get(run_terminal))
        .route("/runs/{id}/sandbox", get(retrieve_run_sandbox))
        .route("/runs/{id}/sandbox/vnc", post(create_sandbox_vnc_preview))
        .route("/runs/{id}/sandbox/services", get(list_sandbox_services))
        .route("/runs/{id}/sandbox/files", get(list_sandbox_files))
        .route(
            "/runs/{id}/sandbox/file",
            get(get_sandbox_file).put(put_sandbox_file),
        )
}

async fn retrieve_run_sandbox(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let record = match load_run_sandbox_instance(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let access = match load_provider_access(&state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    // The record and the status the driver reports for it, in whatever
    // state the sandbox is: a stopped sandbox is described, not started.
    let details = async {
        let sandbox = sandbox_access::attach_run_sandbox(&access, &record, id).await?;
        let status = sandbox.status().await.with_context(|| {
            format!(
                "Failed to describe {} sandbox '{}'",
                record.provider, record.runtime.id
            )
        })?;
        anyhow::Ok(SandboxDetails {
            sandbox: record.clone(),
            status,
        })
    }
    .await;
    match details {
        Ok(details) => Json::<SandboxDetails>(details).into_response(),
        Err(err) => ApiError::new(StatusCode::CONFLICT, format!("{err:#}")).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct SandboxFilesParams {
    path:  String,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(serde::Deserialize)]
struct SandboxFileParams {
    path: String,
}

#[derive(Debug, PartialEq, Eq)]
enum TerminalClientMessage {
    Resize(TerminalSize),
    Close,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TerminalClientControl {
    Resize { cols: u16, rows: u16 },
    Close,
}

fn parse_terminal_control_message(text: &str) -> Result<TerminalClientMessage, &'static str> {
    if text.len() > MAX_TERMINAL_CONTROL_BYTES {
        return Err("Terminal control message is too large.");
    }
    match serde_json::from_str::<TerminalClientControl>(text) {
        Ok(TerminalClientControl::Resize { cols, rows }) => {
            match (NonZeroU16::new(cols), NonZeroU16::new(rows)) {
                (Some(columns), Some(rows)) => Ok(TerminalClientMessage::Resize(TerminalSize {
                    rows,
                    columns,
                })),
                _ => Err("Terminal resize dimensions must be greater than zero."),
            }
        }
        Ok(TerminalClientControl::Close) => Ok(TerminalClientMessage::Close),
        Err(_) => Err("Invalid terminal control message."),
    }
}

fn terminal_server_text(message_type: &str, message: Option<&str>) -> WsMessage {
    let payload = match message {
        Some(message) => serde_json::json!({ "type": message_type, "message": message }),
        None => serde_json::json!({ "type": message_type }),
    };
    WsMessage::Text(payload.to_string().into())
}

#[expect(
    clippy::disallowed_types,
    reason = "The Origin header URL is parsed only for same-origin validation and is never logged."
)]
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) else {
        return true;
    };
    let Some(host) = headers.get("host").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(origin_url) = url::Url::parse(origin) else {
        return false;
    };
    // Parse the Host header through the origin's scheme so default-port
    // normalization is symmetric: browsers omit the port from Host for default
    // scheme ports (e.g. `Host: example.com` on HTTPS) but always include
    // scheme+host in Origin.
    let Ok(host_url) = url::Url::parse(&format!("{}://{host}", origin_url.scheme())) else {
        return false;
    };
    origin_url.host_str() == host_url.host_str()
        && origin_url.port_or_known_default() == host_url.port_or_known_default()
}

async fn run_terminal(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if !origin_allowed(&headers) {
        return ApiError::new(StatusCode::FORBIDDEN, "WebSocket origin is not allowed.")
            .into_response();
    }
    ws.on_upgrade(move |socket| terminal_websocket(socket, state, id))
}

async fn terminal_websocket(mut socket: WebSocket, state: Arc<AppState>, id: RunId) {
    let record = match load_run_sandbox_instance(&state, &id).await {
        Ok(record) => record,
        Err(response) => {
            let message = terminal_error_from_status(response.status());
            let _ = socket
                .send(terminal_server_text("error", Some(&message)))
                .await;
            return;
        }
    };
    let access = match load_provider_access(&state).await {
        Ok(value) => value,
        Err(response) => {
            let _ = socket
                .send(terminal_server_text(
                    "error",
                    Some("Secret store unavailable."),
                ))
                .await;
            tracing::error!(status = %response.status(), "Loading Daytona API key failed");
            return;
        }
    };
    // An interactive login shell in the run's working directory, over the
    // driver's exec with a terminal, on the sandbox brought back to running.
    // Input, resizes and output travel through the exec's controls; ending
    // the session terminates the shell.
    //
    // TODO(parity): every sandbox-driver built-in provider (Host, Docker and
    // Daytona) refuses `ExecSpec.tty`, so the terminal opens only on a
    // provider that supports it; elsewhere the browser gets the provider's
    // refusal as the terminal's error.
    let sandbox = match sandbox_access::attach_running_run_sandbox(&access, &record, id).await {
        Ok(sandbox) => sandbox.sandbox,
        Err(err) => {
            let _ = socket
                .send(terminal_server_text("error", Some(&format!("{err:#}"))))
                .await;
            return;
        }
    };
    let (mut input, stdin) = tokio_io::duplex(64 * 1024);
    let (output_tx, mut output) = mpsc::channel::<Vec<u8>>(64);
    let sink: OutputSink = Arc::new(move |_stream, bytes| {
        let output_tx = output_tx.clone();
        Box::pin(async move {
            // A closed browser ends the session; the shell's output has
            // nowhere to go.
            let _ = output_tx.send(bytes).await;
            Ok::<(), DriverError>(())
        })
    });
    let (resize, resized) = watch::channel(DEFAULT_TERMINAL_SIZE);
    let term = CancellationToken::new();
    let mut spec = ExecSpec::argv(["bash", "-l"]);
    spec.tty = true;
    spec.working_directory = Some(record.runtime.working_directory.clone());
    let controls = ExecControls {
        stdin: Some(Box::pin(stdin)),
        sink: Some(sink),
        term: Some(term.clone()),
        resize: Some(resized),
        ..ExecControls::default()
    };
    let shell = {
        let sandbox = Arc::clone(&sandbox);
        tokio::spawn(async move { sandbox.exec(spec, controls).await })
    };
    let mut shell = std::pin::pin!(shell);

    if socket
        .send(terminal_server_text("ready", None))
        .await
        .is_err()
    {
        term.cancel();
        return;
    }

    loop {
        tokio::select! {
            message = socket.recv() => {
                let Some(message) = message else {
                    break;
                };
                match message {
                    Ok(WsMessage::Binary(bytes)) => {
                        if input.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                    Ok(WsMessage::Text(text)) => {
                        match parse_terminal_control_message(text.as_str()) {
                            Ok(TerminalClientMessage::Resize(size)) => {
                                let _ = resize.send(size);
                            }
                            Ok(TerminalClientMessage::Close) => {
                                let _ = socket.send(terminal_server_text("closed", None)).await;
                                break;
                            }
                            Err(message) => {
                                let _ = socket.send(terminal_server_text("error", Some(message))).await;
                            }
                        }
                    }
                    Ok(WsMessage::Close(_)) => break,
                    Ok(WsMessage::Ping(_) | WsMessage::Pong(_)) => {}
                    Err(err) => {
                        tracing::debug!(error = %err, run_id = %id, "run terminal websocket closed with error");
                        break;
                    }
                }
            }
            Some(bytes) = output.recv() => {
                if socket.send(WsMessage::Binary(bytes.into())).await.is_err() {
                    break;
                }
            }
            ended = &mut shell => {
                // Output the shell wrote before it ended goes first.
                while let Ok(bytes) = output.try_recv() {
                    let _ = socket.send(WsMessage::Binary(bytes.into())).await;
                }
                let message = match ended {
                    Ok(Ok(_)) => terminal_server_text("closed", None),
                    Ok(Err(err)) => {
                        let err = anyhow::Error::new(err).context("Failed to open sandbox terminal");
                        terminal_server_text("error", Some(&format!("{err:#}")))
                    }
                    Err(err) => terminal_server_text("error", Some(&err.to_string())),
                };
                let _ = socket.send(message).await;
                return;
            }
        }
    }
    term.cancel();
    if let Ok(Err(err)) = shell.await {
        tracing::warn!(error = %display_for_log(&err, &SecretRedactor), run_id = %id, "run terminal session ended with an error");
    }
}

fn terminal_error_from_status(status: StatusCode) -> String {
    status
        .canonical_reason()
        .unwrap_or("Terminal unavailable")
        .to_string()
}

async fn generate_preview_url(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<PreviewUrlRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let Ok(port) = u16::try_from(request.port) else {
        return ApiError::bad_request("Port must fit in a u16.").into_response();
    };
    if i32::try_from(request.expires_in_secs.get()).is_err() {
        return ApiError::bad_request("Preview expiry exceeds supported range.").into_response();
    }

    let record = match load_run_sandbox_instance(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let sandbox = match reconnect_run_sandbox_instance(&state, &id, &record).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let Some(port) = NonZeroU16::new(port) else {
        return ApiError::bad_request("Port must be greater than zero.").into_response();
    };
    // A port the provider cannot expose is refused by the provider.
    let options = Expose {
        signed: request.signed,
        ttl_ms: request
            .signed
            .then(|| request.expires_in_secs.get().saturating_mul(1000)),
    };
    let response = match sandbox.sandbox.ports().expose(port, options).await {
        Ok(exposure) => PreviewUrlResponse {
            token: (!request.signed)
                .then(|| exposure.headers.get(PREVIEW_TOKEN_HEADER).cloned())
                .flatten(),
            url:   exposure.url,
        },
        Err(err) => {
            return ApiError::new(StatusCode::CONFLICT, err.to_string()).into_response();
        }
    };

    (StatusCode::CREATED, Json(response)).into_response()
}

async fn create_ssh_access(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SshAccessRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let record = match load_run_sandbox_instance(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };

    let sandbox = match reconnect_run_sandbox_instance(&state, &id, &record).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    // Providers with a leased SSH gateway honor the requested lifetime;
    // providers with a fixed local command return it as is. A provider
    // without endpoints has no access command at all.
    //
    // TODO(parity): of sandbox-driver's built-in providers only Docker
    // offers endpoints; its Daytona provider has no SSH gateway and Host has
    // no access command, and protocol 3 plugins cannot offer endpoints at
    // all. On those, SSH access is refused.
    let result = match sandbox.sandbox.endpoints() {
        Some(endpoints) => {
            let ttl_ms = Duration::from_secs_f64(request.ttl_minutes * 60.0).as_millis();
            match endpoints
                .ssh(u64::try_from(ttl_ms).unwrap_or(u64::MAX))
                .await
            {
                Ok(access) => Ok(Some(access.command)),
                Err(DriverError::Unsupported { .. }) => endpoints
                    .shell_command()
                    .await
                    .map(Some)
                    .context("Failed to build sandbox shell command"),
                Err(err) => Err(anyhow::Error::new(err).context("Failed to create SSH access")),
            }
        }
        None => Ok(None),
    };
    match result {
        Ok(Some(command)) => {
            (StatusCode::CREATED, Json(SshAccessResponse { command })).into_response()
        }
        Ok(None) => ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox provider does not support access commands.",
        )
        .into_response(),
        Err(err) => ApiError::new(StatusCode::CONFLICT, format!("{err:#}")).into_response(),
    }
}

async fn create_sandbox_vnc_preview(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let record = match load_run_sandbox_instance(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let sandbox = match reconnect_run_sandbox_instance(&state, &id, &record).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    // TODO(parity): sandbox-driver's Daytona provider offers no endpoints,
    // so it has no VNC preview, and protocol 3 plugins cannot offer one.
    if sandbox.sandbox.endpoints().is_none() {
        return ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "Sandbox provider does not support VNC previews.",
        )
        .into_response();
    }
    match build_vnc_preview_response(&record.provider, &sandbox.sandbox).await {
        Ok(response) => (StatusCode::CREATED, Json(response)).into_response(),
        Err(response) => response,
    }
}

async fn build_vnc_preview_response(
    provider: &SandboxProviderKind,
    sandbox: &impl VncSandbox,
) -> Result<VncPreviewResponse, Response> {
    let url = sandbox
        .vnc_viewer_url()
        .await
        .map_err(|err| ApiError::new(StatusCode::CONFLICT, format!("{err:#}")).into_response())?;
    let url = vnc_viewer_url(&url)
        .map_err(|err| ApiError::new(StatusCode::CONFLICT, format!("{err:#}")).into_response())?;
    Ok(VncPreviewResponse {
        expires_in_secs: NonZeroU64::new(
            u64::try_from(DEFAULT_VNC_TTL_SECS).expect("default VNC TTL should fit in u64"),
        )
        .expect("default VNC TTL should be nonzero"),
        port: NonZeroU64::new(u64::from(DEFAULT_VNC_NO_VNC_PORT))
            .expect("default VNC port should be nonzero"),
        provider: provider.to_string(),
        url,
    })
}

/// Pins the viewer URL to the noVNC page with autoconnect and scaling. The
/// provider already points at the viewer; this makes the query idempotent
/// so a URL that already carries the viewer parameters is not duplicated.
fn vnc_viewer_url(signed_url: &str) -> anyhow::Result<String> {
    // Internal URL manipulation, not logging — `DisplaySafeUrl` is for
    // logging/error boundaries. The signed URL may carry a credential, so
    // the parse-failure message intentionally omits it.
    #[expect(
        clippy::disallowed_types,
        reason = "internal url manipulation; redaction handled by omitting the URL from error messages"
    )]
    let mut url = url::Url::parse(signed_url).context("Failed to parse signed VNC URL")?;
    let preserved: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != VNC_VIEWER_AUTOCONNECT.0 && key != VNC_VIEWER_RESIZE.0)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_path(VNC_VIEWER_PATH);
    url.set_query(None);
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in &preserved {
            pairs.append_pair(key, value);
        }
        pairs
            .append_pair(VNC_VIEWER_AUTOCONNECT.0, VNC_VIEWER_AUTOCONNECT.1)
            .append_pair(VNC_VIEWER_RESIZE.0, VNC_VIEWER_RESIZE.1);
    }
    Ok(url.into())
}

async fn list_sandbox_files(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFilesParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (record, sandbox) = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    // To `depth` (1, the immediate children, by default), sorted by path,
    // with sizes for files only and names relative to the listed directory.
    // sandbox-driver counts the immediate children as depth 0 and names each
    // entry by its path in the workload.
    let path = resolve_path(&params.path, &record.runtime.working_directory);
    let depth = u32::try_from(params.depth.unwrap_or(1))
        .unwrap_or(u32::MAX)
        .saturating_sub(1);
    let prefix = format!("{}/", path.trim_end_matches('/'));
    match sandbox.sandbox.files().list(&path, depth).await {
        Ok(mut entries) => {
            entries.sort_by(|left, right| left.path.cmp(&right.path));
            Json(SandboxFileListResponse {
                data: entries
                    .into_iter()
                    .map(|entry| SandboxFileEntry {
                        is_dir: entry.kind == FileKind::Directory,
                        size:   (entry.kind == FileKind::File)
                            .then_some(entry.size)
                            .map(u64::cast_signed),
                        name:   entry
                            .path
                            .strip_prefix(&prefix)
                            .map_or_else(|| entry.path.clone(), str::to_owned),
                    })
                    .collect(),
            })
            .into_response()
        }
        Err(err) => ApiError::new(
            StatusCode::NOT_FOUND,
            display_for_log(&err, &SecretRedactor),
        )
        .into_response(),
    }
}

async fn list_sandbox_services(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let record = match load_run_sandbox_instance(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let sandbox = match reconnect_run_sandbox_instance(&state, &id, &record).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let ports = match sandbox_ports::listening_ports(sandbox.sandbox.as_ref()).await {
        Ok(ports) => ports,
        Err(err) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                format!("{LIST_SANDBOX_SERVICES_FAILURE_LABEL} failed: {err:#}"),
            )
            .into_response();
        }
    };
    Json(SandboxServiceListResponse {
        data: services_from_ports(ports, &sandbox.descriptor.ports),
    })
    .into_response()
}

/// The sandbox's listeners grouped by port, previewable ports first: those
/// the provider's descriptor says it can expose.
fn services_from_ports(ports: Vec<ListeningPort>, exposable: &PortSupport) -> Vec<SandboxService> {
    let mut services = BTreeMap::<u16, SandboxService>::new();
    for listener in ports {
        push_service(
            &mut services,
            exposable,
            listener.port,
            listener.address,
            listener.process,
        );
    }
    sorted_services(services)
}

fn sorted_services(services: BTreeMap<u16, SandboxService>) -> Vec<SandboxService> {
    let mut services = services.into_values().collect::<Vec<_>>();
    services.sort_by_key(|service| (!service.preview_supported, service.port));
    services
}

fn push_service(
    services: &mut BTreeMap<u16, SandboxService>,
    exposable: &PortSupport,
    port: u16,
    address: String,
    process: Option<String>,
) {
    let service = services.entry(port).or_insert_with(|| SandboxService {
        port,
        addresses: Vec::new(),
        processes: Vec::new(),
        preview_supported: preview_supported(exposable, port),
    });
    push_unique(&mut service.addresses, address);
    if let Some(process) = process {
        push_unique(&mut service.processes, process);
    }
}

fn preview_supported(exposable: &PortSupport, port: u16) -> bool {
    match exposable {
        PortSupport::Any => port != 0,
        PortSupport::Selected(ports) => {
            NonZeroU16::new(port).is_some_and(|port| ports.contains(&port))
        }
        PortSupport::None => false,
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

async fn get_sandbox_file(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFileParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (record, sandbox) = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let path = resolve_path(&params.path, &record.runtime.working_directory);
    let mut stream = match sandbox.sandbox.files().get(&path, None).await {
        Ok(stream) => stream,
        Err(err) => {
            return ApiError::new(
                StatusCode::NOT_FOUND,
                display_for_log(&err, &SecretRedactor),
            )
            .into_response();
        }
    };
    let mut bytes = Vec::new();
    match stream.read_to_end(&mut bytes).await {
        Ok(_) => octet_stream_response(bytes.into()),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn put_sandbox_file(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFileParams>,
    body: Bytes,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let (record, sandbox) = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let path = resolve_path(&params.path, &record.runtime.working_directory);
    match sandbox
        .sandbox
        .files()
        .put(&path, Box::pin(Cursor::new(body.to_vec())))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            display_for_log(&err, &SecretRedactor),
        )
        .into_response(),
    }
}

/// The run's sandbox record and its handle, attached and running.
async fn reconnect_run_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<(RunSandboxInstance, RunSandbox), Response> {
    let record = load_run_sandbox_instance(state, run_id).await?;
    let sandbox = reconnect_run_sandbox_instance(state, run_id, &record).await?;
    Ok((record, sandbox))
}

/// Reconnects a run's sandbox and brings it to running.
async fn reconnect_run_sandbox_instance(
    state: &Arc<AppState>,
    run_id: &RunId,
    record: &RunSandboxInstance,
) -> Result<RunSandbox, Response> {
    let access = load_provider_access(state).await?;
    sandbox_access::attach_running_run_sandbox(&access, record, *run_id)
        .await
        .map_err(|err| ApiError::new(StatusCode::CONFLICT, format!("{err:#}")).into_response())
}

async fn load_provider_access(state: &AppState) -> Result<ProviderAccess, Response> {
    state.provider_access().await.map_err(|err| {
        tracing::error!(error = ?err, "Loading Daytona API key failed");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secret store operation failed",
        )
        .into_response()
    })
}

async fn load_run_sandbox_instance(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<fabro_types::RunSandboxInstance, Response> {
    let projection = state
        .load_run_projection(run_id)
        .await
        .map_err(IntoResponse::into_response)?;
    projection
        .sandbox
        .clone()
        .and_then(fabro_types::RunSandbox::into_instance)
        .ok_or_else(|| ApiError::not_found("Run sandbox was not created.").into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use futures_util::FutureExt;

    use super::*;

    #[test]
    fn terminal_control_accepts_resize_and_close() {
        assert_eq!(
            parse_terminal_control_message(r#"{"type":"resize","cols":120,"rows":32}"#),
            Ok(TerminalClientMessage::Resize(TerminalSize {
                rows:    NonZeroU16::new(32).unwrap(),
                columns: NonZeroU16::new(120).unwrap(),
            }))
        );
        assert_eq!(
            parse_terminal_control_message(r#"{"type":"close"}"#),
            Ok(TerminalClientMessage::Close)
        );
    }

    #[test]
    fn terminal_control_rejects_malformed_oversized_and_zero_resize() {
        assert!(parse_terminal_control_message("{").is_err());
        assert!(parse_terminal_control_message(r#"{"type":"resize","cols":0,"rows":32}"#).is_err());
        assert!(
            parse_terminal_control_message(&"x".repeat(MAX_TERMINAL_CONTROL_BYTES + 1)).is_err()
        );
    }

    #[test]
    fn origin_validation_allows_absent_and_same_origin() {
        assert!(origin_allowed(&HeaderMap::new()));

        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:4187"));
        headers.insert("origin", HeaderValue::from_static("http://127.0.0.1:4187"));
        assert!(origin_allowed(&headers));
    }

    #[test]
    fn origin_validation_allows_default_https_port_omitted_from_host() {
        // Browsers omit the port from Host when connecting on default scheme ports.
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("example.com"));
        headers.insert("origin", HeaderValue::from_static("https://example.com"));
        assert!(origin_allowed(&headers));

        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("100.53.109.177"));
        headers.insert("origin", HeaderValue::from_static("https://100.53.109.177"));
        assert!(origin_allowed(&headers));
    }

    #[test]
    fn origin_validation_allows_default_http_port_omitted_from_host() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("example.com"));
        headers.insert("origin", HeaderValue::from_static("http://example.com"));
        assert!(origin_allowed(&headers));
    }

    #[test]
    fn origin_validation_allows_explicit_default_port_in_host() {
        // RFC-legal but uncommon: client includes the default port explicitly.
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("example.com:443"));
        headers.insert("origin", HeaderValue::from_static("https://example.com"));
        assert!(origin_allowed(&headers));
    }

    #[test]
    fn origin_validation_rejects_cross_origin_browser_origin() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:4187"));
        headers.insert("origin", HeaderValue::from_static("https://evil.example"));
        assert!(!origin_allowed(&headers));
    }

    #[test]
    fn origin_validation_rejects_scheme_mismatch_on_default_port() {
        // Same hostname but Origin uses http (default port 80) while Host carries the
        // HTTPS default port 443 — different effective ports must not match.
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("example.com:443"));
        headers.insert("origin", HeaderValue::from_static("http://example.com"));
        assert!(!origin_allowed(&headers));
    }

    #[test]
    fn listening_ports_group_by_port_and_sort_previewable_first() {
        let mut node = ListeningPort::new(3000, "127.0.0.1:3000");
        node.process = Some("node".to_string());
        let mut node_v6 = ListeningPort::new(3000, "[::]:3000");
        node_v6.process = Some("node".to_string());
        let mut debug = ListeningPort::new(2500, "[::1]:2500");
        debug.process = Some("pid=168".to_string());
        let services = services_from_ports(
            vec![
                debug,
                node,
                node_v6,
                ListeningPort::new(5173, "0.0.0.0:5173"),
            ],
            &PortSupport::Selected(
                [3000, 5173]
                    .into_iter()
                    .filter_map(NonZeroU16::new)
                    .collect(),
            ),
        );

        assert_eq!(services, vec![
            SandboxService {
                port:              3000,
                addresses:         vec!["127.0.0.1:3000".to_string(), "[::]:3000".to_string()],
                processes:         vec!["node".to_string()],
                preview_supported: true,
            },
            SandboxService {
                port:              5173,
                addresses:         vec!["0.0.0.0:5173".to_string()],
                processes:         vec![],
                preview_supported: true,
            },
            SandboxService {
                port:              2500,
                addresses:         vec!["[::1]:2500".to_string()],
                processes:         vec!["pid=168".to_string()],
                preview_supported: false,
            },
        ]);
    }

    #[test]
    fn preview_support_follows_the_providers_exposable_ports() {
        let selected =
            PortSupport::Selected([3000].into_iter().filter_map(NonZeroU16::new).collect());
        assert!(preview_supported(&selected, 3000));
        assert!(!preview_supported(&selected, 3001));
        assert!(preview_supported(&PortSupport::Any, 10000));
        assert!(!preview_supported(&PortSupport::Any, 0));
        assert!(!preview_supported(&PortSupport::None, 3000));
    }

    struct FakeVncSandbox {
        error:      Option<&'static str>,
        viewer_url: &'static str,
    }

    impl VncSandbox for FakeVncSandbox {
        fn vnc_viewer_url(&self) -> futures_util::future::BoxFuture<'_, anyhow::Result<String>> {
            async move {
                match self.error {
                    Some(message) => Err(anyhow::anyhow!("{message}")),
                    None => Ok(self.viewer_url.to_string()),
                }
            }
            .boxed()
        }
    }

    #[tokio::test]
    async fn vnc_preview_response_uses_daytona_defaults() {
        let sandbox = FakeVncSandbox {
            error:      None,
            viewer_url: "https://preview.example.test/vnc.html?autoconnect=true&resize=scale",
        };

        let response = build_vnc_preview_response(&SandboxProviderKind::DAYTONA, &sandbox)
            .await
            .unwrap();

        assert_eq!(
            response.url,
            "https://preview.example.test/vnc.html?autoconnect=true&resize=scale"
        );
        assert_eq!(response.provider, "daytona");
        assert_eq!(response.port.get(), u64::from(DEFAULT_VNC_NO_VNC_PORT));
        assert_eq!(
            response.expires_in_secs.get(),
            u64::try_from(DEFAULT_VNC_TTL_SECS).unwrap()
        );
    }

    #[test]
    fn vnc_viewer_url_replaces_path_and_appends_viewer_query() {
        let url = super::vnc_viewer_url("https://6080-preview.example.test/").expect("parse");
        assert_eq!(
            url,
            "https://6080-preview.example.test/vnc.html?autoconnect=true&resize=scale"
        );
    }

    #[test]
    fn vnc_viewer_url_preserves_existing_query_params() {
        // Daytona signed previews can carry tokens or other params; viewer
        // params must be appended without dropping them.
        let url =
            super::vnc_viewer_url("https://6080-preview.example.test/?token=abc").expect("parse");
        assert_eq!(
            url,
            "https://6080-preview.example.test/vnc.html?token=abc&autoconnect=true&resize=scale"
        );
    }

    #[test]
    fn vnc_viewer_url_returns_error_for_unparseable_input() {
        assert!(super::vnc_viewer_url("not a url").is_err());
    }

    #[tokio::test]
    async fn vnc_preview_response_maps_provider_failure_to_conflict() {
        let sandbox = FakeVncSandbox {
            error:      Some("computer use failed"),
            viewer_url: "https://preview.example.test/sandbox/6080",
        };

        let response = build_vnc_preview_response(&SandboxProviderKind::DAYTONA, &sandbox)
            .await
            .unwrap_err();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn vnc_viewer_url_does_not_duplicate_viewer_parameters() {
        let url = super::vnc_viewer_url(
            "https://6080-preview.example.test/vnc.html?token=abc&autoconnect=true&resize=scale",
        )
        .expect("parse");
        assert_eq!(
            url,
            "https://6080-preview.example.test/vnc.html?token=abc&autoconnect=true&resize=scale"
        );
    }
}

#[cfg(test)]
mod retrieve_sandbox_tests {
    use std::collections::BTreeMap;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use fabro_types::{RunId, WorkflowPath, WorkflowVersion};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::test_support::{build_test_router, test_app_state, test_register_workflow_version};

    const MINIMAL_DOT: &str = r#"digraph Test {
    graph [goal="Test"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    start -> exit
}"#;

    fn req_get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("sandbox details GET request should build")
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should fit in memory");
        serde_json::from_slice(&bytes).expect("response body should be valid JSON")
    }

    /// Create a run through the API. A run that has not started has a
    /// planned sandbox and nothing else.
    async fn create_run(app: &Router) -> RunId {
        let entrypoint = WorkflowPath::new("workflow.fabro").expect("entrypoint should parse");
        let files = BTreeMap::from([(entrypoint.clone(), MINIMAL_DOT.to_string())]);
        let version = WorkflowVersion::new(entrypoint, files, BTreeMap::new())
            .expect("workflow version should build");
        let workflow_version_id = test_register_workflow_version(app, &version, None).await;
        let intent = json!({
            "workflow_version_id": workflow_version_id,
            "target": { "kind": "none" },
            "args": {}
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(intent.to_string()))
                    .expect("run creation request should build"),
            )
            .await
            .expect("run creation should route");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        body["id"]
            .as_str()
            .expect("run creation response should carry the run id")
            .parse()
            .expect("run id should parse")
    }

    async fn assert_sandbox_not_created_response(response: axum::response::Response) {
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert!(
            body["errors"][0]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("Run sandbox was not created."),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn missing_run_returns_404() {
        let app = build_test_router(test_app_state());
        let absent = RunId::new();
        let response = app
            .oneshot(req_get(&format!("/api/v1/runs/{absent}/sandbox")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert!(
            body["errors"][0]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("Run not found"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn planned_sandbox_returns_404_from_details_endpoint() {
        let app = build_test_router(test_app_state());
        let run_id = create_run(&app).await;
        let response = app
            .oneshot(req_get(&format!("/api/v1/runs/{run_id}/sandbox")))
            .await
            .unwrap();
        assert_sandbox_not_created_response(response).await;
    }

    #[tokio::test]
    async fn planned_sandbox_rejects_live_operations() {
        let app = build_test_router(test_app_state());
        let run_id = create_run(&app).await;

        for uri in [
            format!("/api/v1/runs/{run_id}/sandbox/services"),
            format!("/api/v1/runs/{run_id}/sandbox/files?path=/workspace"),
            format!("/api/v1/runs/{run_id}/sandbox/file?path=/workspace/README.md"),
        ] {
            let response = app.clone().oneshot(req_get(&uri)).await.unwrap();
            assert_sandbox_not_created_response(response).await;
        }
    }
}
