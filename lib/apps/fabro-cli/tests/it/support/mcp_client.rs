//! A stdio MCP client for the tests of fabro's own MCP server.
//!
//! Production agents reach MCP servers through pebble, which owns the client.
//! Fabro's `fabro mcp` command *is* an MCP server, and its tests need a
//! client to speak to it over its standard streams; this is that client and
//! nothing more.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use fabro_types::settings::run::{McpServerSettings, McpTransport};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientInfo, Implementation,
    ProtocolVersion,
};
use rmcp::service::{RoleClient, RunningService, serve_client};
use rmcp::transport::child_process::TokioChildProcess;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time;

enum State {
    Connecting(Option<TokioChildProcess>),
    Ready(Arc<RunningService<RoleClient, ClientInfo>>),
    Closed,
}

/// A test client over a stdio MCP server.
pub(crate) struct McpStdioTestClient {
    server_name: String,
    state:       Mutex<State>,
}

impl McpStdioTestClient {
    /// Spawns the server `config` names. Only a `stdio` transport is
    /// supported; call [`initialize`](Self::initialize) next.
    pub(crate) fn new(config: &McpServerSettings) -> Result<Self> {
        let McpTransport::Stdio { command, env } = &config.transport else {
            return Err(anyhow!(
                "MCP test client '{}': only a stdio transport is supported",
                config.name
            ));
        };
        let (program, args) = command
            .split_first()
            .ok_or_else(|| anyhow!("MCP server '{}': command must not be empty", config.name))?;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if config.clear_env {
            cmd.env_clear();
        }
        if !env.is_empty() {
            cmd.envs(env);
        }
        if let Some(current_dir) = config.current_dir.as_ref() {
            cmd.current_dir(current_dir);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let transport = TokioChildProcess::new(cmd)
            .with_context(|| format!("failed to spawn MCP server '{}'", config.name))?;
        Ok(Self {
            server_name: config.name.clone(),
            state:       Mutex::new(State::Connecting(Some(transport))),
        })
    }

    /// Performs the MCP handshake within `timeout`.
    pub(crate) async fn initialize(&self, timeout: Duration) -> Result<()> {
        let transport = {
            let mut guard = self.state.lock().await;
            match &mut *guard {
                State::Connecting(transport) => transport
                    .take()
                    .ok_or_else(|| anyhow!("client already initializing"))?,
                State::Ready(_) => return Err(anyhow!("client already initialized")),
                State::Closed => return Err(anyhow!("MCP client is shut down")),
            }
        };
        let info = ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new("fabro-mcp-test", env!("CARGO_PKG_VERSION")),
        )
        .with_protocol_version(ProtocolVersion::V_2025_03_26);
        let service = time::timeout(timeout, serve_client(info, transport))
            .await
            .map_err(|_| {
                anyhow!(
                    "timed out initializing MCP server '{}' after {timeout:?}",
                    self.server_name
                )
            })?
            .map_err(|error| {
                anyhow!(
                    "failed to initialize MCP server '{}': {error}",
                    self.server_name
                )
            })?;
        if let Some(peer) = service.peer().peer_info() {
            tracing::info!(
                server = %self.server_name,
                server_name = %peer.server_info.name,
                server_version = %peer.server_info.version,
                "MCP server initialized"
            );
        }
        *self.state.lock().await = State::Ready(Arc::new(service));
        Ok(())
    }

    /// Every tool the server exposes, as `(name, description, input_schema)`.
    pub(crate) async fn list_tools(&self) -> Result<Vec<(String, String, serde_json::Value)>> {
        let service = self.service().await?;
        let tools = service.list_all_tools().await.map_err(|error| {
            anyhow!(
                "failed to list tools from MCP server '{}': {error}",
                self.server_name
            )
        })?;
        Ok(tools
            .into_iter()
            .map(|tool| {
                (
                    tool.name.to_string(),
                    tool.description.as_deref().unwrap_or("").to_string(),
                    serde_json::to_value(&*tool.input_schema).unwrap_or_default(),
                )
            })
            .collect())
    }

    /// Calls `name` with `arguments`, waiting at most `timeout`.
    pub(crate) async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
        timeout: Duration,
    ) -> Result<CallToolResult> {
        let service = self.service().await?;
        let mut params = CallToolRequestParams::new(name.to_string());
        match arguments {
            serde_json::Value::Object(map) => params = params.with_arguments(map),
            serde_json::Value::Null => {}
            other => {
                return Err(anyhow!(
                    "MCP tool arguments must be a JSON object, got {other}"
                ));
            }
        }
        time::timeout(timeout, service.call_tool(params))
            .await
            .map_err(|_| {
                anyhow!(
                    "timed out calling tool '{name}' on MCP server '{}' after {timeout:?}",
                    self.server_name
                )
            })?
            .map_err(|error| {
                anyhow!(
                    "failed to call tool '{name}' on MCP server '{}': {error}",
                    self.server_name
                )
            })
    }

    /// Ends the session and stops the server.
    pub(crate) async fn shutdown(self) -> Result<()> {
        let service = match std::mem::replace(&mut *self.state.lock().await, State::Closed) {
            State::Connecting(_) | State::Closed => None,
            State::Ready(service) => Some(service),
        };
        if let Some(service) = service {
            match Arc::try_unwrap(service) {
                Ok(mut service) => {
                    service
                        .close_with_timeout(Duration::from_secs(2))
                        .await
                        .context("failed to shut down MCP client")?;
                }
                Err(service) => service.cancellation_token().cancel(),
            }
        }
        Ok(())
    }

    async fn service(&self) -> Result<Arc<RunningService<RoleClient, ClientInfo>>> {
        match &*self.state.lock().await {
            State::Ready(service) => Ok(Arc::clone(service)),
            State::Connecting(_) => Err(anyhow!("MCP client not initialized")),
            State::Closed => Err(anyhow!("MCP client is shut down")),
        }
    }
}
