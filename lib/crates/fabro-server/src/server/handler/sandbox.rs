use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU64;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use fabro_sandbox::{TerminalSize, open_terminal_for_run};
use fabro_types::{SandboxServiceDiscoverySource, SandboxServiceListMeta};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use super::super::{
    ApiError, AppState, Bytes, DaytonaSandbox, EnvVars, HeaderMap, IntoResponse, Json,
    NamedTempFile, Path, PreviewUrlRequest, PreviewUrlResponse, Query, RequiredUser, Response,
    Router, RunId, Sandbox, SandboxDetails, SandboxFileEntry, SandboxFileListResponse,
    SandboxProvider, SandboxService, SandboxServiceListResponse, SshAccessRequest,
    SshAccessResponse, State, StatusCode, VncPreviewResponse, collect_causes, fs, get,
    octet_stream_response, parse_run_id_path, post, reconnect_for_run, reject_if_archived,
    render_with_causes, sandbox_details,
};

const MAX_TERMINAL_CONTROL_BYTES: usize = 4096;
const DEFAULT_VNC_NO_VNC_PORT: u16 = 6080;
const DEFAULT_VNC_TTL_SECS: i32 = 3600;
const LIST_SANDBOX_SERVICES_COMMAND: &str = r#"if command -v ss >/dev/null 2>&1; then
  ss -H -ltnp && exit 0
fi
printf 'FABRO_PROC_NET_TCP procfs\n'
for file in /proc/net/tcp /proc/net/tcp6; do
  if [ -r "$file" ]; then
    printf 'FABRO_PROC_NET_TCP %s\n' "$file"
    while IFS= read -r line; do
      printf '%s\n' "$line"
    done < "$file"
  fi
done"#;
const LIST_SANDBOX_SERVICES_FAILURE_LABEL: &str = "sandbox service discovery command";
const LIST_SANDBOX_SERVICES_TIMEOUT_MS: u64 = 5_000;
// Daytona's signed preview points at the noVNC service root, which serves a
// directory listing. Force the iframe to the actual viewer page with
// autoconnect+scale so the user lands on the desktop, not a file index.
const VNC_VIEWER_PATH: &str = "/vnc.html";
const VNC_VIEWER_AUTOCONNECT: (&str, &str) = ("autoconnect", "true");
const VNC_VIEWER_RESIZE: (&str, &str) = ("resize", "scale");

trait VncSandbox {
    fn start_computer_use(&self) -> BoxFuture<'_, fabro_sandbox::Result<()>>;
    fn signed_preview_url(
        &self,
        port: u16,
        expires_in_secs: i32,
    ) -> BoxFuture<'_, fabro_sandbox::Result<String>>;
}

impl VncSandbox for DaytonaSandbox {
    fn start_computer_use(&self) -> BoxFuture<'_, fabro_sandbox::Result<()>> {
        async move {
            let computer_use = self.computer_use().await?;
            computer_use
                .start()
                .await
                .map_err(|err| fabro_sandbox::Error::context("Failed to start Computer Use", err))
                .map(|_| ())
        }
        .boxed()
    }

    fn signed_preview_url(
        &self,
        port: u16,
        expires_in_secs: i32,
    ) -> BoxFuture<'_, fabro_sandbox::Result<String>> {
        async move {
            self.get_signed_preview_url(port, Some(expires_in_secs))
                .await
                .map(|preview| preview.url)
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
    let record = match load_run_sandbox_or_not_found(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    let daytona_organization_id = state.vault_or_env(EnvVars::DAYTONA_ORGANIZATION_ID);
    match sandbox_details(&record, daytona_api_key, daytona_organization_id, Some(id)).await {
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
        Ok(TerminalClientControl::Resize { cols, rows }) if cols > 0 && rows > 0 => {
            Ok(TerminalClientMessage::Resize(TerminalSize { cols, rows }))
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
    let Some(origin_host) = origin_url.host_str() else {
        return false;
    };
    let origin_authority = match origin_url.port_or_known_default() {
        Some(port) => format!("{origin_host}:{port}"),
        None => origin_host.to_string(),
    };
    origin_authority.eq_ignore_ascii_case(host)
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
    let record = match load_run_sandbox(&state, &id).await {
        Ok(record) => record,
        Err(response) => {
            let message = terminal_error_from_status(response.status());
            let _ = socket
                .send(terminal_server_text("error", Some(&message)))
                .await;
            return;
        }
    };
    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    let daytona_organization_id = state.vault_or_env(EnvVars::DAYTONA_ORGANIZATION_ID);
    let session = match open_terminal_for_run(
        &record,
        daytona_api_key,
        daytona_organization_id,
        Some(id),
        TerminalSize::default(),
    )
    .await
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
                                .send(terminal_server_text("error", Some(&err.display_with_causes())))
                                .await;
                            break;
                        }
                    }
                    Ok(WsMessage::Text(text)) => {
                        match parse_terminal_control_message(text.as_str()) {
                            Ok(TerminalClientMessage::Resize(size)) => {
                                if let Err(err) = session.resize(size).await {
                                    let _ = socket
                                        .send(terminal_server_text("error", Some(&err.display_with_causes())))
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
                            .send(terminal_server_text("error", Some(&err.display_with_causes())))
                            .await;
                        break;
                    }
                }
            }
        }
    }
    if let Err(err) = session.close().await {
        tracing::warn!(error = %err.display_with_causes(), run_id = %id, "failed to close run terminal session");
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
    let Ok(expires_in_secs) = i32::try_from(request.expires_in_secs.get()) else {
        return ApiError::bad_request("Preview expiry exceeds supported range.").into_response();
    };

    let sandbox = match reconnect_daytona_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };

    let response = if request.signed {
        match sandbox
            .get_signed_preview_url(port, Some(expires_in_secs))
            .await
        {
            Ok(preview) => PreviewUrlResponse {
                token: None,
                url:   preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err.display_with_causes())
                    .into_response();
            }
        }
    } else {
        match sandbox.get_preview_link(port).await {
            Ok(preview) => PreviewUrlResponse {
                token: Some(preview.token),
                url:   preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err.display_with_causes())
                    .into_response();
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
    let record = match load_run_sandbox(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };

    match record.provider {
        SandboxProvider::Daytona => {
            let sandbox = match reconnect_daytona_sandbox(&state, &id).await {
                Ok(sandbox) => sandbox,
                Err(response) => return response,
            };
            match sandbox.create_ssh_access(Some(request.ttl_minutes)).await {
                Ok(command) => {
                    (StatusCode::CREATED, Json(SshAccessResponse { command })).into_response()
                }
                Err(err) => {
                    ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
                }
            }
        }
        SandboxProvider::Docker => {
            let sandbox = match reconnect_run_sandbox(&state, &id).await {
                Ok(sandbox) => sandbox,
                Err(response) => return response,
            };
            match sandbox.ssh_access_command().await {
                Ok(Some(command)) => {
                    (StatusCode::CREATED, Json(SshAccessResponse { command })).into_response()
                }
                Ok(None) => ApiError::new(
                    StatusCode::CONFLICT,
                    "Sandbox provider does not support access commands.",
                )
                .into_response(),
                Err(err) => {
                    ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
                }
            }
        }
        SandboxProvider::Local => ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox provider does not support access commands.",
        )
        .into_response(),
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
    let record = match load_run_sandbox(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    if record.provider != SandboxProvider::Daytona {
        return ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "Sandbox provider does not support VNC previews.",
        )
        .into_response();
    }
    let sandbox = match reconnect_daytona_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    match build_vnc_preview_response(&sandbox).await {
        Ok(response) => (StatusCode::CREATED, Json(response)).into_response(),
        Err(response) => response,
    }
}

async fn build_vnc_preview_response(
    sandbox: &impl VncSandbox,
) -> Result<VncPreviewResponse, Response> {
    sandbox.start_computer_use().await.map_err(|err| {
        ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
    })?;
    let signed = sandbox
        .signed_preview_url(DEFAULT_VNC_NO_VNC_PORT, DEFAULT_VNC_TTL_SECS)
        .await
        .map_err(|err| {
            ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
        })?;
    let url = vnc_viewer_url(&signed).map_err(|err| {
        ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
    })?;
    Ok(VncPreviewResponse {
        expires_in_secs: NonZeroU64::new(
            u64::try_from(DEFAULT_VNC_TTL_SECS).expect("default VNC TTL should fit in u64"),
        )
        .expect("default VNC TTL should be nonzero"),
        port: NonZeroU64::new(u64::from(DEFAULT_VNC_NO_VNC_PORT))
            .expect("default VNC port should be nonzero"),
        provider: "daytona".to_string(),
        url,
    })
}

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
    url.set_path(VNC_VIEWER_PATH);
    url.query_pairs_mut()
        .append_pair(VNC_VIEWER_AUTOCONNECT.0, VNC_VIEWER_AUTOCONNECT.1)
        .append_pair(VNC_VIEWER_RESIZE.0, VNC_VIEWER_RESIZE.1);
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
                    is_dir: entry.is_dir,
                    name:   entry.name,
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
    let record = match load_run_sandbox(&state, &id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let provider = record.provider;
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let result = match sandbox
        .exec_command(
            LIST_SANDBOX_SERVICES_COMMAND,
            LIST_SANDBOX_SERVICES_TIMEOUT_MS,
            None,
            None,
            None,
        )
        .await
    {
        Ok(result) => result,
        Err(err) => {
            return ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response();
        }
    };
    if !result.is_success() {
        return ApiError::new(
            StatusCode::CONFLICT,
            sandbox_service_command_failure_detail(&result),
        )
        .into_response();
    }

    let discovery = parse_sandbox_services(&result.stdout, provider);
    Json(SandboxServiceListResponse {
        data: discovery.services,
        meta: SandboxServiceListMeta {
            source: discovery.source,
        },
    })
    .into_response()
}

fn sandbox_service_command_failure_detail(result: &fabro_sandbox::ExecResult) -> String {
    let stderr = result.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = result.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    format!("{LIST_SANDBOX_SERVICES_FAILURE_LABEL} failed")
}

struct SandboxServiceDiscovery {
    services: Vec<SandboxService>,
    source:   SandboxServiceDiscoverySource,
}

fn parse_sandbox_services(output: &str, provider: SandboxProvider) -> SandboxServiceDiscovery {
    if output
        .lines()
        .any(|line| line.trim_start().starts_with("FABRO_PROC_NET_TCP "))
    {
        SandboxServiceDiscovery {
            services: parse_proc_net_listening_services(output, provider),
            source:   SandboxServiceDiscoverySource::Procfs,
        }
    } else {
        SandboxServiceDiscovery {
            services: parse_ss_listening_services(output, provider),
            source:   SandboxServiceDiscoverySource::Ss,
        }
    }
}

fn parse_ss_listening_services(output: &str, provider: SandboxProvider) -> Vec<SandboxService> {
    let mut services = BTreeMap::<u16, SandboxService>::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let Some(address) = fields.get(3).copied() else {
            continue;
        };
        let Some(port) = parse_ss_local_port(address) else {
            continue;
        };
        let process = (fields.len() > 5).then(|| fields[5..].join(" "));
        push_service(&mut services, provider, port, address.to_string(), process);
    }
    sorted_services(services)
}

fn parse_ss_local_port(address: &str) -> Option<u16> {
    let port = address.rsplit_once(':')?.1.parse::<u16>().ok()?;
    (port > 0).then_some(port)
}

#[derive(Clone, Copy)]
enum ProcNetFamily {
    Ipv4,
    Ipv6,
}

fn parse_proc_net_listening_services(
    output: &str,
    provider: SandboxProvider,
) -> Vec<SandboxService> {
    let mut services = BTreeMap::<u16, SandboxService>::new();
    let mut family = None;
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(path) = line.strip_prefix("FABRO_PROC_NET_TCP ") {
            family = if path.ends_with("/tcp6") {
                Some(ProcNetFamily::Ipv6)
            } else {
                Some(ProcNetFamily::Ipv4)
            };
            continue;
        }
        if line.starts_with("sl") {
            continue;
        }
        let Some(family) = family else {
            continue;
        };
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let (Some(local_address), Some(state)) = (fields.get(1), fields.get(3)) else {
            continue;
        };
        if *state != "0A" {
            continue;
        }
        let Some((address, port)) = parse_proc_net_local_address(local_address, family) else {
            continue;
        };
        push_service(&mut services, provider, port, address, None);
    }
    sorted_services(services)
}

fn sorted_services(services: BTreeMap<u16, SandboxService>) -> Vec<SandboxService> {
    let mut services = services.into_values().collect::<Vec<_>>();
    services.sort_by_key(|service| (!service.preview_supported, service.port));
    services
}

fn parse_proc_net_local_address(value: &str, family: ProcNetFamily) -> Option<(String, u16)> {
    let (address_hex, port_hex) = value.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if port == 0 {
        return None;
    }
    let address = match family {
        ProcNetFamily::Ipv4 => format!("{}:{port}", parse_proc_net_ipv4(address_hex)?),
        ProcNetFamily::Ipv6 => format!("[{}]:{port}", parse_proc_net_ipv6(address_hex)?),
    };
    Some((address, port))
}

fn parse_proc_net_ipv4(value: &str) -> Option<Ipv4Addr> {
    if value.len() != 8 {
        return None;
    }
    let raw = u32::from_str_radix(value, 16).ok()?;
    Some(Ipv4Addr::from(raw.to_le_bytes()))
}

fn parse_proc_net_ipv6(value: &str) -> Option<Ipv6Addr> {
    if value.len() != 32 {
        return None;
    }
    let mut bytes = [0_u8; 16];
    for (chunk_index, chunk) in value.as_bytes().chunks_exact(8).enumerate() {
        let chunk = std::str::from_utf8(chunk).ok()?;
        let raw = u32::from_str_radix(chunk, 16).ok()?;
        bytes[chunk_index * 4..chunk_index * 4 + 4].copy_from_slice(&raw.to_le_bytes());
    }
    Some(Ipv6Addr::from(bytes))
}

fn push_service(
    services: &mut BTreeMap<u16, SandboxService>,
    provider: SandboxProvider,
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

fn preview_supported(provider: SandboxProvider, port: u16) -> bool {
    provider == SandboxProvider::Daytona && (3000..=9999).contains(&port)
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
) -> Result<Box<dyn Sandbox>, Response> {
    let record = load_run_sandbox(state, run_id).await?;
    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    let sandbox = reconnect_for_run(&record, daytona_api_key, Some(*run_id))
        .await
        .map_err(|err| {
            let detail = render_with_causes(&err.to_string(), &collect_causes(err.as_ref()));
            ApiError::new(StatusCode::CONFLICT, detail).into_response()
        })?;
    sandbox.start().await.map_err(|err| {
        ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
    })?;
    Ok(sandbox)
}

async fn reconnect_daytona_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<DaytonaSandbox, Response> {
    let record = load_run_sandbox(state, run_id).await?;
    if record.provider != SandboxProvider::Daytona {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox provider does not support this capability.",
        )
        .into_response());
    }
    let Some(runtime) = record.runtime.as_ref() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox record is missing runtime metadata.",
        )
        .into_response());
    };
    let Some(repo_cloned) = runtime.repo_cloned else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox record is missing clone metadata.",
        )
        .into_response());
    };
    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    let sandbox = DaytonaSandbox::reconnect(
        &runtime.id,
        daytona_api_key,
        repo_cloned,
        runtime.working_directory.clone(),
        runtime.clone_origin_url.clone(),
        runtime.clone_branch.clone(),
    )
    .await
    .map_err(|err| {
        ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
    })?;
    sandbox.start().await.map_err(|err| {
        ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response()
    })?;
    Ok(sandbox)
}

async fn load_run_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<fabro_types::RunSandbox, Response> {
    match state.store.open_run_reader(run_id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => run_state.sandbox.ok_or_else(|| {
                ApiError::new(StatusCode::CONFLICT, "Run has no active sandbox.").into_response()
            }),
            Err(err) => Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            ),
        },
        Err(_) => Err(ApiError::not_found("Run not found.").into_response()),
    }
}

/// Same as `load_run_sandbox`, but treats a missing sandbox as
/// `404 Not Found` instead of `409 Conflict`. Used by the inspection endpoint
/// where there is no resource to act on if the run never had a sandbox.
async fn load_run_sandbox_or_not_found(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<fabro_types::RunSandbox, Response> {
    match state.store.open_run_reader(run_id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => run_state
                .sandbox
                .ok_or_else(|| ApiError::not_found("Run has no sandbox.").into_response()),
            Err(err) => Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            ),
        },
        Err(_) => Err(ApiError::not_found("Run not found.").into_response()),
    }
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
    fn origin_validation_rejects_cross_origin_browser_origin() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:4187"));
        headers.insert("origin", HeaderValue::from_static("https://evil.example"));
        assert!(!origin_allowed(&headers));
    }

    #[test]
    fn ss_parser_extracts_addresses_processes_and_preview_support() {
        let services = parse_ss_listening_services(
            r#"
LISTEN 0 4096 127.0.0.1:3000 0.0.0.0:* users:(("node",pid=42,fd=23))
LISTEN 0 4096 0.0.0.0:5173 0.0.0.0:* users:(("vite",pid=84,fd=19))
LISTEN 0 4096 [::]:8080 [::]:* users:(("server",pid=126,fd=9))
LISTEN 0 4096 [::1]:2500 [::]:* users:(("debug",pid=168,fd=7))
"#,
            SandboxProvider::Daytona,
        );

        assert_eq!(services.len(), 4);
        assert_eq!(services[0].port, 3000);
        assert_eq!(services[0].addresses, vec!["127.0.0.1:3000"]);
        assert_eq!(services[0].processes, vec![
            r#"users:(("node",pid=42,fd=23))"#
        ]);
        assert!(services[0].preview_supported);
        assert_eq!(services[1].port, 5173);
        assert_eq!(services[1].addresses, vec!["0.0.0.0:5173"]);
        assert!(services[1].preview_supported);
        assert_eq!(services[2].port, 8080);
        assert_eq!(services[2].addresses, vec!["[::]:8080"]);
        assert!(services[2].preview_supported);
        assert_eq!(services[3].port, 2500);
        assert_eq!(services[3].addresses, vec!["[::1]:2500"]);
        assert_eq!(services[3].processes, vec![
            r#"users:(("debug",pid=168,fd=7))"#
        ]);
        assert!(!services[3].preview_supported);
    }

    #[test]
    fn ss_parser_ignores_malformed_and_non_numeric_ports() {
        let services = parse_ss_listening_services(
            r#"
LISTEN 0 4096 127.0.0.1:not-a-port 0.0.0.0:* users:(("node",pid=42,fd=23))
LISTEN 0 4096 missing-peer
not enough fields
LISTEN 0 4096 127.0.0.1:0 0.0.0.0:* users:(("zero",pid=1,fd=2))
LISTEN 0 4096 127.0.0.1:65536 0.0.0.0:* users:(("large",pid=1,fd=2))
"#,
            SandboxProvider::Daytona,
        );

        assert!(services.is_empty());
    }

    #[test]
    fn ss_parser_groups_duplicate_ports_and_deduplicates_values() {
        let services = parse_ss_listening_services(
            r#"
LISTEN 0 4096 127.0.0.1:3000 0.0.0.0:* users:(("node",pid=42,fd=23))
LISTEN 0 4096 0.0.0.0:3000 0.0.0.0:* users:(("node",pid=42,fd=23))
LISTEN 0 4096 127.0.0.1:3000 0.0.0.0:* users:(("node",pid=42,fd=23))
LISTEN 0 4096 [::]:3000 [::]:* users:(("vite",pid=84,fd=19))
"#,
            SandboxProvider::Daytona,
        );

        assert_eq!(services, vec![SandboxService {
            port:              3000,
            addresses:         vec![
                "127.0.0.1:3000".to_string(),
                "0.0.0.0:3000".to_string(),
                "[::]:3000".to_string(),
            ],
            processes:         vec![
                r#"users:(("node",pid=42,fd=23))"#.to_string(),
                r#"users:(("vite",pid=84,fd=19))"#.to_string(),
            ],
            preview_supported: true,
        }]);
    }

    #[test]
    fn proc_net_parser_extracts_listening_tcp_services_without_processes() {
        let discovery = parse_sandbox_services(
            r"
FABRO_PROC_NET_TCP /proc/net/tcp
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 11111
   1: 00000000:1435 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 22222
   2: 0100007F:2328 00000000:0000 01 00000000:00000000 00:00000000 00000000   501        0 33333
FABRO_PROC_NET_TCP /proc/net/tcp6
  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 44444
   1: 00000000000000000000000001000000:09C4 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 55555
",
            SandboxProvider::Daytona,
        );

        assert_eq!(discovery.source, SandboxServiceDiscoverySource::Procfs);
        assert_eq!(discovery.services, vec![
            SandboxService {
                port:              3000,
                addresses:         vec!["127.0.0.1:3000".to_string()],
                processes:         vec![],
                preview_supported: true,
            },
            SandboxService {
                port:              5173,
                addresses:         vec!["0.0.0.0:5173".to_string()],
                processes:         vec![],
                preview_supported: true,
            },
            SandboxService {
                port:              8080,
                addresses:         vec!["[::]:8080".to_string()],
                processes:         vec![],
                preview_supported: true,
            },
            SandboxService {
                port:              2500,
                addresses:         vec!["[::1]:2500".to_string()],
                processes:         vec![],
                preview_supported: false,
            },
        ]);
    }

    #[test]
    fn preview_support_is_daytona_only_for_documented_range() {
        assert!(!preview_supported(SandboxProvider::Daytona, 2500));
        assert!(preview_supported(SandboxProvider::Daytona, 3000));
        assert!(preview_supported(SandboxProvider::Daytona, 9999));
        assert!(!preview_supported(SandboxProvider::Daytona, 10000));
        assert!(!preview_supported(SandboxProvider::Docker, 3000));
    }

    #[test]
    fn sandbox_service_command_failure_prefers_stderr_then_stdout() {
        let mut result = fabro_sandbox::ExecResult {
            stdout:      "stdout detail".to_string(),
            stderr:      "stderr detail".to_string(),
            exit_code:   Some(127),
            termination: fabro_types::CommandTermination::Exited,
            duration_ms: 10,
        };
        assert_eq!(
            sandbox_service_command_failure_detail(&result),
            "stderr detail"
        );

        result.stderr.clear();
        assert_eq!(
            sandbox_service_command_failure_detail(&result),
            "stdout detail"
        );

        result.stdout.clear();
        assert_eq!(
            sandbox_service_command_failure_detail(&result),
            "sandbox service discovery command failed"
        );
    }

    struct FakeVncSandbox {
        start_error:      Option<&'static str>,
        signed_url_error: Option<&'static str>,
        signed_url:       &'static str,
    }

    impl VncSandbox for FakeVncSandbox {
        fn start_computer_use(
            &self,
        ) -> futures_util::future::BoxFuture<'_, fabro_sandbox::Result<()>> {
            async move {
                match self.start_error {
                    Some(message) => Err(fabro_sandbox::Error::message(message)),
                    None => Ok(()),
                }
            }
            .boxed()
        }

        fn signed_preview_url(
            &self,
            port: u16,
            expires_in_secs: i32,
        ) -> futures_util::future::BoxFuture<'_, fabro_sandbox::Result<String>> {
            async move {
                assert_eq!(port, DEFAULT_VNC_NO_VNC_PORT);
                assert_eq!(expires_in_secs, DEFAULT_VNC_TTL_SECS);
                match self.signed_url_error {
                    Some(message) => Err(fabro_sandbox::Error::message(message)),
                    None => Ok(self.signed_url.to_string()),
                }
            }
            .boxed()
        }
    }

    #[tokio::test]
    async fn vnc_preview_response_uses_daytona_defaults() {
        let sandbox = FakeVncSandbox {
            start_error:      None,
            signed_url_error: None,
            signed_url:       "https://preview.example.test/sandbox/6080",
        };

        let response = build_vnc_preview_response(&sandbox).await.unwrap();

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
    async fn vnc_preview_response_maps_computer_use_start_failure_to_conflict() {
        let sandbox = FakeVncSandbox {
            start_error:      Some("computer use failed"),
            signed_url_error: None,
            signed_url:       "https://preview.example.test/sandbox/6080",
        };

        let response = build_vnc_preview_response(&sandbox).await.unwrap_err();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn vnc_preview_response_maps_signed_preview_failure_to_conflict() {
        let sandbox = FakeVncSandbox {
            start_error:      None,
            signed_url_error: Some("preview failed"),
            signed_url:       "https://preview.example.test/sandbox/6080",
        };

        let response = build_vnc_preview_response(&sandbox).await.unwrap_err();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}

#[cfg(test)]
mod retrieve_sandbox_tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use fabro_types::{Graph, RunId, WorkflowSettings};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::test_support::{build_test_router, test_app_state};

    fn req_get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("sandbox details GET request should build")
    }

    fn req_post(uri: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .body(Body::empty())
            .expect("sandbox POST request should build")
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should fit in memory");
        serde_json::from_slice(&bytes).expect("response body should be valid JSON")
    }

    async fn append_run_created(run_store: &fabro_store::RunDatabase, run_id: &RunId) {
        let payload = fabro_store::EventPayload::new(
            json!({
                "id": "evt-run-created",
                "ts": "2026-05-09T11:59:00Z",
                "run_id": run_id,
                "event": "run.created",
                "properties": {
                    "settings": WorkflowSettings::default(),
                    "graph": Graph::new("test"),
                    "run_dir": "/tmp/test",
                },
            }),
            run_id,
        )
        .expect("run.created payload should validate");
        run_store.append_event(&payload).await.unwrap();
    }

    async fn append_sandbox_initialized(
        run_store: &fabro_store::RunDatabase,
        run_id: &RunId,
        provider: &str,
    ) {
        let payload = fabro_store::EventPayload::new(
            json!({
                "id": "evt-sandbox-init",
                "ts": "2026-05-09T12:00:00Z",
                "run_id": run_id,
                "event": "sandbox.initialized",
                "properties": {
                    "provider": provider,
                    "id": format!("{provider}:sandbox-id"),
                    "working_directory": "/workspace",
                },
            }),
            run_id,
        )
        .expect("sandbox.initialized payload should validate");
        run_store.append_event(&payload).await.unwrap();
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
    async fn run_without_sandbox_runtime_returns_planned_sandbox_details() {
        let state = test_app_state();
        let app = build_test_router(state.clone());
        let run_id = RunId::new();
        let run_store = state
            .store_ref()
            .create_run(&run_id)
            .await
            .expect("test run should be creatable");
        append_run_created(&run_store, &run_id).await;
        let response = app
            .oneshot(req_get(&format!("/api/v1/runs/{run_id}/sandbox")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["sandbox"]["provider"], "local");
        assert!(body["sandbox"]["runtime"].is_null());
    }

    #[tokio::test]
    async fn local_sandbox_returns_provider_neutral_details() {
        let state = test_app_state();
        let app = build_test_router(state.clone());
        let run_id = RunId::new();
        let run_store = state
            .store_ref()
            .create_run(&run_id)
            .await
            .expect("test run should be creatable");
        append_run_created(&run_store, &run_id).await;
        append_sandbox_initialized(&run_store, &run_id, "local").await;

        let response = app
            .oneshot(req_get(&format!("/api/v1/runs/{run_id}/sandbox")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["sandbox"]["provider"], "local");
        assert_eq!(body["sandbox"]["runtime"]["id"], "local:sandbox-id");
        assert_eq!(
            body["sandbox"]["runtime"]["working_directory"],
            "/workspace"
        );
        assert_eq!(body["state"], "running");
        assert!(body.get("name").is_none());
        assert!(body.get("identifier").is_none());
        assert!(body["resources"].is_object());
        assert_eq!(body["network"]["egress"]["mode"], "unknown");
        assert_eq!(body["network"]["ingress"]["mode"], "unknown");
        assert!(body["timestamps"].is_object());
    }

    #[tokio::test]
    async fn local_sandbox_vnc_returns_501() {
        let state = test_app_state();
        let app = build_test_router(state.clone());
        let run_id = RunId::new();
        let run_store = state
            .store_ref()
            .create_run(&run_id)
            .await
            .expect("test run should be creatable");
        append_run_created(&run_store, &run_id).await;
        append_sandbox_initialized(&run_store, &run_id, "local").await;

        let response = app
            .oneshot(req_post(&format!("/api/v1/runs/{run_id}/sandbox/vnc")))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn docker_sandbox_vnc_returns_501_without_reconnect() {
        let state = test_app_state();
        let app = build_test_router(state.clone());
        let run_id = RunId::new();
        let run_store = state
            .store_ref()
            .create_run(&run_id)
            .await
            .expect("test run should be creatable");
        append_run_created(&run_store, &run_id).await;
        append_sandbox_initialized(&run_store, &run_id, "docker").await;

        let response = app
            .oneshot(req_post(&format!("/api/v1/runs/{run_id}/sandbox/vnc")))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }
}
