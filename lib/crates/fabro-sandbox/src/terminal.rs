use async_trait::async_trait;
#[cfg(feature = "daytona")]
use fabro_static::EnvVars;
use fabro_types::{RunId, RunSandboxInstance, SandboxProviderKind};

#[cfg(any(feature = "daytona", feature = "docker"))]
use crate::Sandbox;
#[cfg(feature = "daytona")]
use crate::daytona::{DEFAULT_DAYTONA_API_URL, DaytonaSandbox};
#[cfg(feature = "docker")]
use crate::docker::DockerSandbox;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: 120,
            rows: 32,
        }
    }
}

#[async_trait]
pub trait TerminalSession: Send + Sync {
    async fn write_input(&self, bytes: &[u8]) -> crate::Result<()>;
    async fn read_output(&self) -> crate::Result<Option<Vec<u8>>>;
    async fn resize(&self, size: TerminalSize) -> crate::Result<()>;
    async fn close(&self) -> crate::Result<()>;
}

pub async fn open_terminal_for_run(
    record: &RunSandboxInstance,
    daytona_api_key: Option<String>,
    daytona_organization_id: Option<String>,
    run_id: Option<RunId>,
    size: TerminalSize,
) -> crate::Result<Box<dyn TerminalSession>> {
    #[cfg(any(feature = "daytona", feature = "docker"))]
    let runtime = &record.runtime;
    #[cfg(not(feature = "daytona"))]
    let _ = (&daytona_api_key, &daytona_organization_id);
    #[cfg(not(feature = "docker"))]
    let _ = &run_id;
    #[cfg(not(any(feature = "daytona", feature = "docker")))]
    let _ = size;

    match record.provider {
        #[cfg(feature = "daytona")]
        SandboxProviderKind::Daytona => {
            let repo_cloned = runtime.repo_cloned.ok_or_else(|| {
                crate::Error::message("Daytona run sandbox is missing clone metadata")
            })?;
            let sandbox = DaytonaSandbox::reconnect(
                &runtime.id,
                daytona_api_key.clone(),
                repo_cloned,
                runtime.working_directory.clone(),
                runtime.clone_origin_url.clone(),
                runtime.clone_branch.clone(),
            )
            .await?;
            sandbox.start().await?;
            let api_key = resolve_daytona_api_key(daytona_api_key)?;
            let organization_id = resolve_daytona_organization_id(daytona_organization_id);
            let session = DaytonaTerminalSession::open(
                &sandbox,
                api_key,
                organization_id,
                daytona_api_base_url(),
                size,
            )
            .await?;
            Ok(Box::new(session))
        }
        #[cfg(not(feature = "daytona"))]
        SandboxProviderKind::Daytona => Err(crate::Error::message(
            "Daytona sandbox support is not enabled",
        )),
        #[cfg(feature = "docker")]
        SandboxProviderKind::Docker => {
            let repo_cloned = runtime.repo_cloned.ok_or_else(|| {
                crate::Error::message("Docker run sandbox is missing clone metadata")
            })?;
            let sandbox = DockerSandbox::reconnect(
                &runtime.id,
                repo_cloned,
                runtime.working_directory.clone(),
                runtime.clone_origin_url.clone(),
                runtime.clone_branch.clone(),
                run_id,
            )
            .await?;
            sandbox.start().await?;
            let session = DockerTerminalSession::open(&sandbox, size).await?;
            Ok(Box::new(session))
        }
        #[cfg(not(feature = "docker"))]
        SandboxProviderKind::Docker => Err(crate::Error::message(
            "Docker sandbox support is not enabled",
        )),
        SandboxProviderKind::Local => Err(crate::Error::message(
            "Local sandboxes do not support embedded terminals",
        )),
    }
}

#[cfg(feature = "daytona")]
#[expect(
    clippy::disallowed_methods,
    reason = "Terminal reconnect falls back to the process environment when no vault value was supplied."
)]
fn resolve_daytona_api_key(api_key: Option<String>) -> crate::Result<String> {
    api_key
        .or_else(|| std::env::var(EnvVars::DAYTONA_API_KEY).ok())
        .ok_or_else(|| crate::Error::message("DAYTONA_API_KEY is required for Daytona terminals"))
}

#[cfg(feature = "daytona")]
#[expect(
    clippy::disallowed_methods,
    reason = "Daytona SDK configuration convention uses process environment fallbacks for API URLs."
)]
fn daytona_api_base_url() -> String {
    std::env::var(EnvVars::DAYTONA_API_URL)
        .or_else(|_| std::env::var(EnvVars::DAYTONA_SERVER_URL))
        .unwrap_or_else(|_| DEFAULT_DAYTONA_API_URL.to_string())
}

#[cfg(feature = "daytona")]
#[expect(
    clippy::disallowed_methods,
    reason = "Terminal reconnect falls back to the process environment when no vault value was supplied."
)]
fn resolve_daytona_organization_id(organization_id: Option<String>) -> Option<String> {
    organization_id.or_else(|| std::env::var(EnvVars::DAYTONA_ORGANIZATION_ID).ok())
}

#[cfg(feature = "daytona")]
mod daytona_terminal {
    use std::collections::HashMap;
    use std::sync::Once;

    use async_trait::async_trait;
    use daytona_api_client::apis::configuration::Configuration;
    use daytona_api_client::apis::sandbox_api;
    use futures_util::stream::{SplitSink, SplitStream};
    use futures_util::{SinkExt, StreamExt};
    use rand::Rng;
    use rustls::crypto::ring;
    use serde::{Deserialize, Serialize};
    use tokio::net::TcpStream;
    use tokio::runtime::Handle;
    use tokio::sync::Mutex;
    use tokio_tungstenite::tungstenite::error::ProtocolError;
    use tokio_tungstenite::tungstenite::handshake::client;
    use tokio_tungstenite::tungstenite::http::Request;
    use tokio_tungstenite::tungstenite::protocol::Message as ProviderMessage;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite};

    use super::{TerminalSession, TerminalSize};
    use crate::Sandbox;
    use crate::daytona::DaytonaSandbox;

    type ProviderWs = WebSocketStream<MaybeTlsStream<TcpStream>>;
    type ProviderSink = SplitSink<ProviderWs, ProviderMessage>;
    type ProviderStream = SplitStream<ProviderWs>;

    static RUSTLS_PROVIDER: Once = Once::new();

    pub(super) struct DaytonaTerminalSession {
        toolbox_base_url: String,
        api_key:          String,
        org_id:           Option<String>,
        session_id:       String,
        write:            Mutex<Option<ProviderSink>>,
        read:             Mutex<Option<ProviderStream>>,
        closed:           Mutex<bool>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct DaytonaPtyCreateRequest {
        cols:       u16,
        rows:       u16,
        cwd:        String,
        envs:       HashMap<String, String>,
        id:         String,
        lazy_start: bool,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DaytonaPtyCreateResponse {
        session_id: String,
    }

    #[derive(Serialize)]
    struct DaytonaPtyResizeRequest {
        cols: u16,
        rows: u16,
    }

    impl DaytonaTerminalSession {
        pub(super) async fn open(
            sandbox: &DaytonaSandbox,
            api_key: String,
            org_id: Option<String>,
            api_base_url: String,
            size: TerminalSize,
        ) -> crate::Result<Self> {
            ensure_rustls_provider();
            let sandbox_id = sandbox.daytona_id()?.to_string();
            let toolbox_base_url =
                daytona_toolbox_base_url(&api_base_url, &api_key, org_id.as_deref(), &sandbox_id)
                    .await?;
            let session_id = daytona_terminal_session_id();
            let session_id = create_pty_session(
                &toolbox_base_url,
                &api_key,
                org_id.as_deref(),
                &session_id,
                sandbox.working_directory().to_string(),
                size,
            )
            .await?;
            let ws_url = daytona_pty_ws_url(&toolbox_base_url, &session_id)?;
            let request = daytona_ws_request(&ws_url, &api_key, org_id.as_deref())?;
            let (stream, _) = connect_async(request).await.map_err(|err| {
                crate::Error::context("Failed to connect Daytona terminal WebSocket", err)
            })?;
            let (write, read) = stream.split();
            Ok(Self {
                toolbox_base_url,
                api_key,
                org_id,
                session_id,
                write: Mutex::new(Some(write)),
                read: Mutex::new(Some(read)),
                closed: Mutex::new(false),
            })
        }

        async fn kill_session(&self) -> crate::Result<()> {
            let url = format!(
                "{}/process/pty/{}",
                trim_slash(&self.toolbox_base_url),
                url_component(&self.session_id)
            );
            let mut request = fabro_http::http_client()
                .map_err(|err| crate::Error::context("Failed to build HTTP client", err))?
                .delete(url)
                .bearer_auth(&self.api_key);
            if let Some(org_id) = self.org_id.as_deref() {
                request = request.header("X-Daytona-Organization-ID", org_id);
            }
            let response = request.send().await.map_err(|err| {
                crate::Error::context("Failed to delete Daytona PTY session", err)
            })?;
            if !response.status().is_success()
                && response.status() != fabro_http::StatusCode::NOT_FOUND
            {
                return Err(daytona_response_error(
                    "Failed to delete Daytona PTY session",
                    response,
                )
                .await);
            }
            Ok(())
        }
    }

    #[async_trait]
    impl TerminalSession for DaytonaTerminalSession {
        async fn write_input(&self, bytes: &[u8]) -> crate::Result<()> {
            let mut write = self.write.lock().await;
            let Some(write) = write.as_mut() else {
                return Ok(());
            };
            write
                .send(ProviderMessage::Binary(bytes.to_vec().into()))
                .await
                .map_err(|err| crate::Error::context("Failed to write Daytona terminal input", err))
        }

        async fn read_output(&self) -> crate::Result<Option<Vec<u8>>> {
            let mut read = self.read.lock().await;
            let Some(read) = read.as_mut() else {
                return Ok(None);
            };
            while let Some(message) = read.next().await {
                match message {
                    Ok(ProviderMessage::Binary(bytes)) => return Ok(Some(bytes.to_vec())),
                    Ok(ProviderMessage::Text(text)) => {
                        if is_daytona_terminal_control_text(text.as_str()) {
                            continue;
                        }
                        return Ok(Some(text.as_str().as_bytes().to_vec()));
                    }
                    Ok(ProviderMessage::Close(_))
                    | Err(tungstenite::Error::Protocol(
                        ProtocolError::ResetWithoutClosingHandshake,
                    )) => return Ok(None),
                    Ok(
                        ProviderMessage::Ping(_)
                        | ProviderMessage::Pong(_)
                        | ProviderMessage::Frame(_),
                    ) => {}
                    Err(err) => {
                        return Err(crate::Error::context(
                            "Failed to read Daytona terminal output",
                            err,
                        ));
                    }
                }
            }
            Ok(None)
        }

        async fn resize(&self, size: TerminalSize) -> crate::Result<()> {
            let url = format!(
                "{}/process/pty/{}/resize",
                trim_slash(&self.toolbox_base_url),
                url_component(&self.session_id)
            );
            let mut request = fabro_http::http_client()
                .map_err(|err| crate::Error::context("Failed to build HTTP client", err))?
                .post(url)
                .bearer_auth(&self.api_key)
                .json(&DaytonaPtyResizeRequest {
                    cols: size.cols,
                    rows: size.rows,
                });
            if let Some(org_id) = self.org_id.as_deref() {
                request = request.header("X-Daytona-Organization-ID", org_id);
            }
            let response = request
                .send()
                .await
                .map_err(|err| crate::Error::context("Failed to resize Daytona terminal", err))?;
            if !response.status().is_success() {
                return Err(
                    daytona_response_error("Failed to resize Daytona terminal", response).await,
                );
            }
            Ok(())
        }

        async fn close(&self) -> crate::Result<()> {
            let mut closed = self.closed.lock().await;
            if *closed {
                return Ok(());
            }
            *closed = true;
            drop(closed);

            if let Some(mut write) = self.write.lock().await.take() {
                let _ = write.send(ProviderMessage::Close(None)).await;
            }
            let _ = self.read.lock().await.take();
            self.kill_session().await
        }
    }

    impl Drop for DaytonaTerminalSession {
        fn drop(&mut self) {
            let toolbox_base_url = self.toolbox_base_url.clone();
            let api_key = self.api_key.clone();
            let org_id = self.org_id.clone();
            let session_id = self.session_id.clone();
            if let Ok(handle) = Handle::try_current() {
                handle.spawn(async move {
                    let url = format!(
                        "{}/process/pty/{}",
                        trim_slash(&toolbox_base_url),
                        url_component(&session_id)
                    );
                    let Ok(client) = fabro_http::http_client() else {
                        return;
                    };
                    let mut request = client.delete(url).bearer_auth(api_key);
                    if let Some(org_id) = org_id.as_deref() {
                        request = request.header("X-Daytona-Organization-ID", org_id);
                    }
                    if let Err(err) = request.send().await {
                        tracing::warn!(error = %err, "failed to clean up Daytona terminal session");
                    }
                });
            }
        }
    }

    async fn create_pty_session(
        toolbox_base_url: &str,
        api_key: &str,
        org_id: Option<&str>,
        session_id: &str,
        cwd: String,
        size: TerminalSize,
    ) -> crate::Result<String> {
        let mut envs = HashMap::new();
        envs.insert("TERM".to_string(), "xterm-256color".to_string());
        envs.insert("LANG".to_string(), "C.UTF-8".to_string());
        let url = format!("{}/process/pty", trim_slash(toolbox_base_url));
        let mut request = fabro_http::http_client()
            .map_err(|err| crate::Error::context("Failed to build HTTP client", err))?
            .post(url)
            .bearer_auth(api_key)
            .json(&DaytonaPtyCreateRequest {
                cols: size.cols,
                rows: size.rows,
                cwd,
                envs,
                id: session_id.to_string(),
                lazy_start: false,
            });
        if let Some(org_id) = org_id {
            request = request.header("X-Daytona-Organization-ID", org_id);
        }
        let response = request
            .send()
            .await
            .map_err(|err| crate::Error::context("Failed to create Daytona PTY session", err))?;
        if !response.status().is_success() {
            return Err(
                daytona_response_error("Failed to create Daytona PTY session", response).await,
            );
        }
        let body = response
            .json::<DaytonaPtyCreateResponse>()
            .await
            .map_err(|err| crate::Error::context("Failed to decode Daytona PTY response", err))?;
        Ok(body.session_id)
    }

    async fn daytona_toolbox_base_url(
        api_base_url: &str,
        api_key: &str,
        org_id: Option<&str>,
        sandbox_id: &str,
    ) -> crate::Result<String> {
        let http_client = fabro_http::http_client()
            .map_err(|err| crate::Error::context("Failed to build HTTP client", err))?;
        let configuration = Configuration {
            base_path:           trim_slash(api_base_url).to_string(),
            user_agent:          Some(concat!("fabro-sandbox/", env!("CARGO_PKG_VERSION")).into()),
            client:              reqwest_middleware::ClientBuilder::new(http_client).build(),
            basic_auth:          None,
            oauth_access_token:  None,
            bearer_access_token: Some(api_key.to_string()),
            api_key:             None,
        };
        let proxy_url = sandbox_api::get_toolbox_proxy_url(&configuration, sandbox_id, org_id)
            .await
            .map_err(|err| {
                crate::Error::context("Failed to resolve Daytona toolbox proxy URL", err)
            })?;
        Ok(format!(
            "{}/{}",
            trim_slash(&proxy_url.url),
            url_component(sandbox_id)
        ))
    }

    fn daytona_pty_ws_url(toolbox_base_url: &str, session_id: &str) -> crate::Result<String> {
        let base = trim_slash(toolbox_base_url);
        let ws_base = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            return Err(crate::Error::message(
                "Daytona API URL must start with http:// or https://",
            ));
        };
        Ok(format!(
            "{}/process/pty/{}/connect",
            ws_base,
            url_component(session_id)
        ))
    }

    async fn daytona_response_error(action: &str, response: fabro_http::Response) -> crate::Error {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let trimmed = body.trim();
        if trimmed.is_empty() {
            crate::Error::message(format!("{action}: HTTP {status}"))
        } else {
            crate::Error::message(format!(
                "{action}: HTTP {status}: {}",
                truncate_error_body(trimmed)
            ))
        }
    }

    fn truncate_error_body(body: &str) -> String {
        const MAX_LEN: usize = 500;
        if body.len() <= MAX_LEN {
            return body.to_string();
        }
        format!("{}...", &body[..MAX_LEN])
    }

    fn daytona_terminal_session_id() -> String {
        format!("fabro-terminal-{:016x}", rand::rng().random::<u64>())
    }

    fn is_daytona_terminal_control_text(text: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return false;
        };
        value
            .as_object()
            .and_then(|object| object.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("control")
    }

    fn daytona_ws_request(
        ws_url: &str,
        api_key: &str,
        org_id: Option<&str>,
    ) -> crate::Result<Request<()>> {
        let mut request = Request::builder()
            .uri(ws_url)
            .header("Host", extract_host(ws_url))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", client::generate_key())
            .header("Authorization", format!("Bearer {api_key}"))
            .header("X-Daytona-Source", "fabro");
        if let Some(org_id) = org_id {
            request = request.header("X-Daytona-Organization-ID", org_id);
        }
        request.body(()).map_err(|err| {
            crate::Error::context("Failed to build Daytona terminal WebSocket request", err)
        })
    }

    fn ensure_rustls_provider() {
        RUSTLS_PROVIDER.call_once(|| {
            let _ = ring::default_provider().install_default();
        });
    }

    pub(super) fn trim_slash(value: &str) -> &str {
        value.trim_end_matches('/')
    }

    pub(super) fn url_component(value: &str) -> String {
        value.replace('/', "%2F")
    }

    fn extract_host(ws_url: &str) -> String {
        ws_url
            .strip_prefix("wss://")
            .or_else(|| ws_url.strip_prefix("ws://"))
            .and_then(|rest| rest.split('/').next())
            .unwrap_or_default()
            .to_string()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn builds_daytona_pty_websocket_url() {
            assert_eq!(
                daytona_pty_ws_url("https://proxy.app.daytona.io/toolbox/sandbox%2Fa", "pty-1")
                    .unwrap(),
                "wss://proxy.app.daytona.io/toolbox/sandbox%2Fa/process/pty/pty-1/connect"
            );
        }

        #[tokio::test]
        async fn create_daytona_pty_session_posts_to_toolbox_proxy_with_id() {
            let server = httpmock::MockServer::start_async().await;
            let create = server
                .mock_async(|when, then| {
                    when.method(httpmock::Method::POST)
                        .path("/toolbox/sandbox-1/process/pty")
                        .header("authorization", "Bearer dtn_test")
                        .json_body(serde_json::json!({
                            "cols": 120,
                            "rows": 32,
                            "cwd": "/home/daytona/workspace",
                            "envs": {
                                "TERM": "xterm-256color",
                                "LANG": "C.UTF-8"
                            },
                            "id": "fabro-terminal-test",
                            "lazyStart": false
                        }));
                    then.status(200)
                        .header("content-type", "application/json")
                        .json_body(serde_json::json!({
                            "sessionId": "fabro-terminal-test"
                        }));
                })
                .await;

            let session_id = create_pty_session(
                &format!("{}/toolbox/sandbox-1", server.base_url()),
                "dtn_test",
                None,
                "fabro-terminal-test",
                "/home/daytona/workspace".to_string(),
                TerminalSize {
                    cols: 120,
                    rows: 32,
                },
            )
            .await
            .unwrap();

            assert_eq!(session_id, "fabro-terminal-test");
            create.assert_async().await;
        }

        #[tokio::test]
        async fn resolves_daytona_toolbox_proxy_base_url() {
            let server = httpmock::MockServer::start_async().await;
            let proxy = server
                .mock_async(|when, then| {
                    when.method(httpmock::Method::GET)
                        .path("/sandbox/sandbox-1/toolbox-proxy-url")
                        .header("authorization", "Bearer dtn_test")
                        .header("X-Daytona-Organization-ID", "org-1");
                    then.status(200)
                        .header("content-type", "application/json")
                        .json_body(serde_json::json!({
                            "url": format!("{}/toolbox", server.base_url())
                        }));
                })
                .await;

            let toolbox_base_url = daytona_toolbox_base_url(
                &server.base_url(),
                "dtn_test",
                Some("org-1"),
                "sandbox-1",
            )
            .await
            .unwrap();

            assert_eq!(
                toolbox_base_url,
                format!("{}/toolbox/sandbox-1", server.base_url())
            );
            proxy.assert_async().await;
        }

        #[test]
        fn identifies_daytona_terminal_control_text() {
            assert!(is_daytona_terminal_control_text(
                r#"{"status":"connected","type":"control"}"#
            ));
            assert!(is_daytona_terminal_control_text(
                r#"{"type":"control","status":"resized"}"#
            ));
            assert!(!is_daytona_terminal_control_text("hello\n"));
            assert!(!is_daytona_terminal_control_text(
                r#"{"type":"output","text":"hello"}"#
            ));
            assert!(!is_daytona_terminal_control_text("{"));
        }
    }
}

#[cfg(feature = "daytona")]
use daytona_terminal::DaytonaTerminalSession;

#[cfg(feature = "docker")]
mod docker_terminal {
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;
    use bollard::Docker;
    use bollard::container::LogOutput;
    use bollard::errors::Error as DockerError;
    use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
    use futures::{Stream, StreamExt};
    use tokio::io::{AsyncWrite, AsyncWriteExt};
    use tokio::sync::Mutex;

    use super::{TerminalSession, TerminalSize};
    use crate::Sandbox;
    use crate::docker::DockerSandbox;

    type DockerInput = Pin<Box<dyn AsyncWrite + Send>>;
    type DockerOutput = Pin<Box<dyn Stream<Item = Result<LogOutput, DockerError>> + Send>>;

    pub(super) struct DockerTerminalSession {
        docker:       Docker,
        container_id: String,
        exec_id:      String,
        pid_file:     String,
        input:        Mutex<Option<DockerInput>>,
        output:       Mutex<Option<DockerOutput>>,
        closed:       Mutex<bool>,
    }

    impl DockerTerminalSession {
        pub(super) async fn open(
            sandbox: &DockerSandbox,
            size: TerminalSize,
        ) -> crate::Result<Self> {
            let docker = sandbox.docker_client();
            let container_id = sandbox.container_identifier()?.to_string();
            let pid_file = format!("/tmp/fabro-terminal-{}.pid", uuid_fragment());
            let exec_opts = docker_terminal_exec_options(sandbox.working_directory(), &pid_file);
            let exec = docker
                .create_exec(&container_id, exec_opts)
                .await
                .map_err(|err| {
                    crate::Error::context("Failed to create Docker terminal exec", err)
                })?;
            let exec_id = exec.id;
            let start = docker.start_exec(&exec_id, None).await.map_err(|err| {
                crate::Error::context("Failed to start Docker terminal exec", err)
            })?;
            let StartExecResults::Attached { output, input } = start else {
                return Err(crate::Error::message("Docker terminal exec did not attach"));
            };
            docker
                .resize_exec(&exec_id, ResizeExecOptions {
                    height: size.rows,
                    width:  size.cols,
                })
                .await
                .map_err(|err| {
                    crate::Error::context("Failed to resize Docker terminal exec", err)
                })?;
            Ok(Self {
                docker,
                container_id,
                exec_id,
                pid_file,
                input: Mutex::new(Some(input)),
                output: Mutex::new(Some(output)),
                closed: Mutex::new(false),
            })
        }

        async fn kill_shell(&self) -> crate::Result<()> {
            let command = format!(
                "if [ -f {pid_file} ]; then kill -TERM \"$(cat {pid_file})\" 2>/dev/null || true; rm -f {pid_file}; fi",
                pid_file = crate::shell_quote(&self.pid_file),
            );
            let exec = self
                .docker
                .create_exec(&self.container_id, CreateExecOptions {
                    cmd: Some(vec!["sh".to_string(), "-lc".to_string(), command]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    ..Default::default()
                })
                .await
                .map_err(|err| {
                    crate::Error::context("Failed to create Docker terminal cleanup exec", err)
                })?;
            self.docker
                .start_exec(&exec.id, None)
                .await
                .map_err(|err| {
                    crate::Error::context("Failed to run Docker terminal cleanup exec", err)
                })?;
            Ok(())
        }
    }

    #[async_trait]
    impl TerminalSession for DockerTerminalSession {
        async fn write_input(&self, bytes: &[u8]) -> crate::Result<()> {
            let mut input = self.input.lock().await;
            let Some(input) = input.as_mut() else {
                return Ok(());
            };
            input
                .write_all(bytes)
                .await
                .map_err(|err| crate::Error::context("Failed to write Docker terminal input", err))
        }

        async fn read_output(&self) -> crate::Result<Option<Vec<u8>>> {
            let mut output = self.output.lock().await;
            let Some(output) = output.as_mut() else {
                return Ok(None);
            };
            match output.next().await {
                Some(Ok(chunk)) => Ok(Some(chunk.into_bytes().to_vec())),
                Some(Err(err)) => Err(crate::Error::context(
                    "Failed to read Docker terminal output",
                    err,
                )),
                None => Ok(None),
            }
        }

        async fn resize(&self, size: TerminalSize) -> crate::Result<()> {
            self.docker
                .resize_exec(&self.exec_id, ResizeExecOptions {
                    height: size.rows,
                    width:  size.cols,
                })
                .await
                .map_err(|err| crate::Error::context("Failed to resize Docker terminal exec", err))
        }

        async fn close(&self) -> crate::Result<()> {
            let mut closed = self.closed.lock().await;
            if *closed {
                return Ok(());
            }
            *closed = true;
            drop(closed);
            let _ = self.input.lock().await.take();
            let _ = self.output.lock().await.take();
            self.kill_shell().await
        }
    }

    fn docker_terminal_exec_options(
        working_directory: &str,
        pid_file: &str,
    ) -> CreateExecOptions<String> {
        let command = format!(
            "printf '%s\\n' $$ > {pid_file}; exec sh -l",
            pid_file = crate::shell_quote(pid_file),
        );
        CreateExecOptions {
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(true),
            cmd: Some(vec!["sh".to_string(), "-lc".to_string(), command]),
            working_dir: Some(working_directory.to_string()),
            env: Some(vec![
                "TERM=xterm-256color".to_string(),
                "LANG=C.UTF-8".to_string(),
            ]),
            ..Default::default()
        }
    }

    static DOCKER_TERMINAL_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn uuid_fragment() -> String {
        format!(
            "{:016x}",
            DOCKER_TERMINAL_COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn docker_terminal_exec_options_attach_tty_and_workspace_env() {
            let options = docker_terminal_exec_options("/workspace", "/tmp/fabro-terminal.pid");
            assert_eq!(options.attach_stdin, Some(true));
            assert_eq!(options.attach_stdout, Some(true));
            assert_eq!(options.attach_stderr, Some(true));
            assert_eq!(options.tty, Some(true));
            assert_eq!(options.working_dir.as_deref(), Some("/workspace"));
            assert_eq!(
                options.env,
                Some(vec![
                    "TERM=xterm-256color".to_string(),
                    "LANG=C.UTF-8".to_string()
                ])
            );
            assert!(options.cmd.unwrap().join(" ").contains("exec sh -l"));
        }
    }
}

#[cfg(feature = "docker")]
use docker_terminal::DockerTerminalSession;
