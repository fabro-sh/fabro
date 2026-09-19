use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use fabro_sandbox::{
    FileKind, ProviderAccess, PtySize, RunSandbox, open_terminal_for_run, reconnect_for_run,
};
use fabro_types::{RunSandboxInstance, SandboxProviderKind};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use sandbox_driver::{ListeningPort, Services as _};

use super::super::{
    ApiError, AppState, Bytes, HeaderMap, IntoResponse, Json, NamedTempFile, Path,
    PreviewUrlRequest, PreviewUrlResponse, Query, RequiredUser, Response, Router, RunId,
    SandboxDetails, SandboxFileEntry, SandboxFileListResponse, SandboxService,
    SandboxServiceListResponse, SshAccessRequest, SshAccessResponse, State, StatusCode,
    VncPreviewResponse, collect_causes, fs, get, octet_stream_response, parse_run_id_path, post,
    reject_if_archived, render_with_causes, sandbox_details,
};

const MAX_TERMINAL_CONTROL_BYTES: usize = 4096;
const DEFAULT_VNC_NO_VNC_PORT: u16 = 6080;
const DEFAULT_VNC_TTL_SECS: i32 = 3600;
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
    fn vnc_viewer_url(&self) -> BoxFuture<'_, fabro_sandbox::Result<String>>;
}

impl VncSandbox for RunSandbox {
    fn vnc_viewer_url(&self) -> BoxFuture<'_, fabro_sandbox::Result<String>> {
        async move {
            let vnc = self.handle()?.vnc().ok_or_else(|| {
                fabro_sandbox::Error::message("Sandbox provider does not support VNC previews.")
            })?;
            vnc.vnc_connection()
                .await
                .map(|connection| connection.url)
                .map_err(|err| fabro_sandbox::Error::context("Failed to open a VNC preview", err))
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
    match sandbox_details(&record, &access, Some(id)).await {
        Ok(details) => Json::<SandboxDetails>(details).into_response(),
        Err(err) => {
            let detail = format!("{err:#}");
            let status = if detail.contains("has no details implementation") {
                StatusCode::NOT_IMPLEMENTED
            } else {
                StatusCode::CONFLICT
            };
            ApiError::new(status, detail).into_response()
        }
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
    Resize(PtySize),
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
        Ok(TerminalClientControl::Resize { cols, rows }) if cols > 0 && rows > 0 => {
            Ok(TerminalClientMessage::Resize(PtySize { cols, rows }))
        }
        Ok(TerminalClientControl::Resize { .. }) => {
            Err("Terminal resize dimensions must be greater than zero.")
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
    let session = match open_terminal_for_run(&record, &access, Some(id), PtySize::default()).await
    {
        Ok(session) => session,
        Err(err) => {
            let _ = socket
                .send(terminal_server_text(
                    "error",
                    Some(&err.display_with_causes()),
                ))
                .await;
            return;
        }
    };

    if socket
        .send(terminal_server_text("ready", None))
        .await
        .is_err()
    {
        let _ = session.close().await;
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
                        if let Err(err) = session.write_input(&bytes).await {
                            let _ = socket
                                .send(terminal_server_text("error", Some(&fabro_sandbox::display_for_log(&err))))
                                .await;
                            break;
                        }
                    }
                    Ok(WsMessage::Text(text)) => {
                        match parse_terminal_control_message(text.as_str()) {
                            Ok(TerminalClientMessage::Resize(size)) => {
                                if let Err(err) = session.resize(size).await {
                                    let _ = socket
                                        .send(terminal_server_text("error", Some(&fabro_sandbox::display_for_log(&err))))
                                        .await;
                                    break;
                                }
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
            output = session.read_output() => {
                match output {
                    Ok(Some(bytes)) => {
                        if socket.send(WsMessage::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = socket.send(terminal_server_text("closed", None)).await;
                        break;
                    }
                    Err(err) => {
                        let _ = socket
                            .send(terminal_server_text("error", Some(&fabro_sandbox::display_for_log(&err))))
                            .await;
                        break;
                    }
                }
            }
        }
    }
    if let Err(err) = session.close().await {
        tracing::warn!(error = %fabro_sandbox::display_for_log(&err), run_id = %id, "failed to close run terminal session");
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
    let handle = match sandbox.handle() {
        Ok(handle) => handle,
        Err(err) => {
            return ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response();
        }
    };
    let Some(previews) = handle.preview_urls() else {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox provider does not support preview URLs.",
        )
        .into_response();
    };

    let response = if request.signed {
        match previews
            .signed_preview_url(port, Duration::from_secs(request.expires_in_secs.get()))
            .await
        {
            Ok(preview) => PreviewUrlResponse {
                token: None,
                url:   preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err.to_string()).into_response();
            }
        }
    } else {
        match previews.preview_url(port).await {
            Ok(preview) => PreviewUrlResponse {
                token: preview.headers.get(PREVIEW_TOKEN_HEADER).cloned(),
                url:   preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err.to_string()).into_response();
            }
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

    if record.provider == SandboxProviderKind::LOCAL {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox provider does not support access commands.",
        )
        .into_response();
    }
    let sandbox = match reconnect_run_sandbox_instance(&state, &id, &record).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let handle = match sandbox.handle() {
        Ok(handle) => handle,
        Err(err) => {
            return ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response();
        }
    };
    // Providers with a leased SSH gateway honor the requested lifetime;
    // providers with a fixed local command return it as is.
    let result = match handle.ssh() {
        Some(ssh) => ssh
            .ssh_access(Some(Duration::from_secs_f64(request.ttl_minutes * 60.0)))
            .await
            .map(|access| Some(access.command))
            .map_err(|err| fabro_sandbox::Error::context("Failed to create SSH access", err)),
        None => sandbox.ssh_access_command().await,
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
        Err(err) => ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response(),
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
    if record.provider != SandboxProviderKind::DAYTONA {
        return ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "Sandbox provider does not support VNC previews.",
        )
        .into_response();
    }
    let sandbox = match reconnect_run_sandbox_instance(&state, &id, &record).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    match build_vnc_preview_response(&record.provider, &sandbox).await {
        Ok(response) => (StatusCode::CREATED, Json(response)).into_response(),
        Err(response) => response,
    }
}

async fn build_vnc_preview_response(
    provider: &SandboxProviderKind,
    sandbox: &impl VncSandbox,
) -> Result<VncPreviewResponse, Response> {
    let url = sandbox.vnc_viewer_url().await.map_err(|err| {
        ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
    })?;
    let url = vnc_viewer_url(&url).map_err(|err| {
        ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
    })?;
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
fn vnc_viewer_url(signed_url: &str) -> fabro_sandbox::Result<String> {
    // Internal URL manipulation, not logging — `DisplaySafeUrl` is for
    // logging/error boundaries. The signed URL may carry a credential, so
    // the parse-failure message intentionally omits it.
    #[expect(
        clippy::disallowed_types,
        reason = "internal url manipulation; redaction handled by omitting the URL from error messages"
    )]
    let mut url = url::Url::parse(signed_url)
        .map_err(|err| fabro_sandbox::Error::context("Failed to parse signed VNC URL", err))?;
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
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    match sandbox.list_directory(&params.path, params.depth).await {
        Ok(entries) => Json(SandboxFileListResponse {
            data: entries
                .into_iter()
                .map(|entry| SandboxFileEntry {
                    is_dir: entry.kind == FileKind::Directory,
                    name:   entry.path,
                    size:   entry.size.map(u64::cast_signed),
                })
                .collect(),
        })
        .into_response(),
        Err(err) => ApiError::new(StatusCode::NOT_FOUND, err.display_with_causes()).into_response(),
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
    let provider = record.provider.clone();
    let sandbox = match reconnect_run_sandbox_instance(&state, &id, &record).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let services = match sandbox.services() {
        Ok(services) => services,
        Err(err) => {
            return ApiError::new(StatusCode::NOT_IMPLEMENTED, err.display_with_causes())
                .into_response();
        }
    };
    let ports = match services.listening_ports().await {
        Ok(ports) => ports,
        Err(err) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                format!("{LIST_SANDBOX_SERVICES_FAILURE_LABEL} failed: {err}"),
            )
            .into_response();
        }
    };
    Json(SandboxServiceListResponse {
        data: services_from_ports(ports, &provider),
    })
    .into_response()
}

/// The driver's listeners grouped by port, previewable ports first.
fn services_from_ports(
    ports: Vec<ListeningPort>,
    provider: &SandboxProviderKind,
) -> Vec<SandboxService> {
    let mut services = BTreeMap::<u16, SandboxService>::new();
    for listener in ports {
        push_service(
            &mut services,
            provider,
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
    provider: &SandboxProviderKind,
    port: u16,
    address: String,
    process: Option<String>,
) {
    let service = services.entry(port).or_insert_with(|| SandboxService {
        port,
        addresses: Vec::new(),
        processes: Vec::new(),
        preview_supported: preview_supported(provider, port),
    });
    push_unique(&mut service.addresses, address);
    if let Some(process) = process {
        push_unique(&mut service.processes, process);
    }
}

fn preview_supported(provider: &SandboxProviderKind, port: u16) -> bool {
    *provider == SandboxProviderKind::DAYTONA && (3000..=9999).contains(&port)
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
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let temp = match NamedTempFile::new() {
        Ok(temp) => temp,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) = sandbox
        .download_file_to_local(&params.path, temp.path())
        .await
    {
        return ApiError::new(StatusCode::NOT_FOUND, err.display_with_causes()).into_response();
    }
    match fs::read(temp.path()).await {
        Ok(bytes) => octet_stream_response(bytes.into()),
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
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let temp = match NamedTempFile::new() {
        Ok(temp) => temp,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) = fs::write(temp.path(), &body).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    match sandbox
        .upload_file_from_local(temp.path(), &params.path)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.display_with_causes())
            .into_response(),
    }
}

async fn reconnect_run_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<RunSandbox, Response> {
    let record = load_run_sandbox_instance(state, run_id).await?;
    reconnect_run_sandbox_instance(state, run_id, &record).await
}

/// Reconnects a run's sandbox and brings it to running.
async fn reconnect_run_sandbox_instance(
    state: &Arc<AppState>,
    run_id: &RunId,
    record: &RunSandboxInstance,
) -> Result<RunSandbox, Response> {
    let access = load_provider_access(state).await?;
    let sandbox = reconnect_for_run(record, &access, Some(*run_id), None)
        .await
        .map_err(|err| {
            let detail = render_with_causes(&err.to_string(), &collect_causes(err.as_ref()));
            ApiError::new(StatusCode::CONFLICT, detail).into_response()
        })?;
    sandbox.activate().await.map_err(|err| {
        ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
    })?;
    Ok(sandbox)
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
            Ok(TerminalClientMessage::Resize(PtySize {
                cols: 120,
                rows: 32,
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
            &SandboxProviderKind::DAYTONA,
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
    fn preview_support_is_daytona_only_for_documented_range() {
        assert!(!preview_supported(&SandboxProviderKind::DAYTONA, 2500));
        assert!(preview_supported(&SandboxProviderKind::DAYTONA, 3000));
        assert!(preview_supported(&SandboxProviderKind::DAYTONA, 9999));
        assert!(!preview_supported(&SandboxProviderKind::DAYTONA, 10000));
        assert!(!preview_supported(&SandboxProviderKind::DOCKER, 3000));
    }

    struct FakeVncSandbox {
        error:      Option<&'static str>,
        viewer_url: &'static str,
    }

    impl VncSandbox for FakeVncSandbox {
        fn vnc_viewer_url(
            &self,
        ) -> futures_util::future::BoxFuture<'_, fabro_sandbox::Result<String>> {
            async move {
                match self.error {
                    Some(message) => Err(fabro_sandbox::Error::message(message)),
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
