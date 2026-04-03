use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService, serve_client};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time;
use tracing::{debug, error, info, warn};

use crate::client_handler::LoggingClientHandler;
use crate::config::{McpServerSettings, McpTransport};

enum ClientState {
    /// Transport created but handshake not yet performed.
    Connecting(Option<PendingTransport>),
    /// Handshake complete, ready for tool calls.
    Ready(Arc<RunningService<RoleClient, LoggingClientHandler>>),
}

enum PendingTransport {
    Stdio(TokioChildProcess),
    Http(StreamableHttpClientTransport<reqwest::Client>),
}

/// MCP client wrapping the rmcp SDK. Handles stdio and HTTP transports.
pub struct McpClient {
    server_name: String,
    state: Mutex<ClientState>,
}

impl McpClient {
    /// Create a new MCP client from config. Does not connect yet — call `initialize()`.
    pub fn new(config: &McpServerSettings) -> Result<Self> {
        let transport = match &config.transport {
            McpTransport::Stdio { command, env } => {
                let (program, args) = command.split_first().ok_or_else(|| {
                    anyhow!("MCP server '{}': command must not be empty", config.name)
                })?;
                let mut cmd = Command::new(program);
                cmd.args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);

                if !env.is_empty() {
                    cmd.envs(env);
                }

                #[cfg(unix)]
                cmd.process_group(0);

                let transport = TokioChildProcess::new(cmd)
                    .map_err(|e| anyhow!("failed to spawn MCP server '{}': {}", config.name, e))?;

                PendingTransport::Stdio(transport)
            }
            McpTransport::Http { url, headers } => {
                let http_config = StreamableHttpClientTransportConfig::with_uri(url.clone());

                let mut builder = reqwest::Client::builder();
                if !headers.is_empty() {
                    let mut header_map = HeaderMap::new();
                    for (key, value) in headers {
                        let name = HeaderName::from_bytes(key.as_bytes())
                            .map_err(|e| anyhow!("invalid header name '{key}': {e}"))?;
                        let val = HeaderValue::from_str(value)
                            .map_err(|e| anyhow!("invalid header value for '{key}': {e}"))?;
                        header_map.insert(name, val);
                    }
                    builder = builder.default_headers(header_map);
                }

                let http_client = builder.build()?;
                let transport =
                    StreamableHttpClientTransport::with_client(http_client, http_config);

                PendingTransport::Http(transport)
            }
            McpTransport::Sandbox { .. } => {
                return Err(anyhow!(
                    "MCP server '{}': Sandbox transport must be resolved to Http before connecting",
                    config.name
                ));
            }
        };

        let transport_type = match &transport {
            PendingTransport::Stdio(_) => "stdio",
            PendingTransport::Http(_) => "http",
        };
        debug!(server = %config.name, transport = transport_type, "Creating MCP client");

        Ok(Self {
            server_name: config.name.clone(),
            state: Mutex::new(ClientState::Connecting(Some(transport))),
        })
    }

    /// Perform the initialization handshake with the MCP server.
    pub async fn initialize(&self, timeout: Duration) -> Result<()> {
        let handler = LoggingClientHandler;

        let service = {
            let mut guard = self.state.lock().await;
            let transport = match &mut *guard {
                ClientState::Connecting(t) => t
                    .take()
                    .ok_or_else(|| anyhow!("client already initializing"))?,
                ClientState::Ready(_) => return Err(anyhow!("client already initialized")),
            };

            // Drop the lock before the blocking handshake
            drop(guard);

            debug!(server = %self.server_name, "Starting MCP server handshake");

            let handshake = async {
                match transport {
                    PendingTransport::Stdio(t) => serve_client(handler.clone(), t).await,
                    PendingTransport::Http(t) => serve_client(handler.clone(), t).await,
                }
            };

            let service = time::timeout(timeout, handshake)
                .await
                .map_err(|_| {
                    error!(server = %self.server_name, timeout_secs = timeout.as_secs(), "MCP server handshake timed out");
                    anyhow!(
                        "timed out initializing MCP server '{}' after {:?}",
                        self.server_name,
                        timeout
                    )
                })?
                .map_err(|e| {
                    error!(server = %self.server_name, error = %e, "MCP server handshake failed");
                    anyhow!(
                        "failed to initialize MCP server '{}': {}",
                        self.server_name,
                        e
                    )
                })?;

            let peer_info = service.peer().peer_info();
            if let Some(info) = peer_info {
                info!(
                    server = %self.server_name,
                    server_name = %info.server_info.name,
                    server_version = %info.server_info.version,
                    "MCP server initialized"
                );
            }

            Arc::new(service)
        };

        let mut guard = self.state.lock().await;
        *guard = ClientState::Ready(service);
        Ok(())
    }

    /// List all tools exposed by this server.
    /// Returns `(name, description, input_schema)` tuples.
    pub async fn list_tools(&self) -> Result<Vec<(String, String, serde_json::Value)>> {
        let service = self.service().await?;
        let result = service.list_all_tools().await.map_err(|e| {
            anyhow!(
                "failed to list tools from MCP server '{}': {}",
                self.server_name,
                e
            )
        })?;

        let tools: Vec<_> = result
            .into_iter()
            .map(|tool| {
                let name = tool.name.to_string();
                let description = tool.description.as_deref().unwrap_or("").to_string();
                let input_schema = serde_json::to_value(&*tool.input_schema).unwrap_or_default();
                (name, description, input_schema)
            })
            .collect();

        debug!(server = %self.server_name, tool_count = tools.len(), "Listed MCP server tools");

        Ok(tools)
    }

    /// Call a tool on this server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
        timeout: Duration,
    ) -> Result<CallToolResult> {
        let service = self.service().await?;

        let args = match arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => {
                return Err(anyhow!(
                    "MCP tool arguments must be a JSON object, got {other}"
                ));
            }
        };

        let mut params = CallToolRequestParams::new(name.to_string());
        if let Some(arguments) = args {
            params = params.with_arguments(arguments);
        }

        debug!(server = %self.server_name, tool = %name, "Calling MCP tool");

        let result = time::timeout(timeout, service.call_tool(params))
            .await
            .map_err(|_| {
                warn!(server = %self.server_name, tool = %name, timeout_secs = timeout.as_secs(), "MCP tool call timed out");
                anyhow!(
                    "timed out calling tool '{}' on MCP server '{}' after {:?}",
                    name,
                    self.server_name,
                    timeout
                )
            })?
            .map_err(|e| {
                anyhow!(
                    "failed to call tool '{}' on MCP server '{}': {}",
                    name,
                    self.server_name,
                    e
                )
            })?;

        Ok(result)
    }

    async fn service(&self) -> Result<Arc<RunningService<RoleClient, LoggingClientHandler>>> {
        let guard = self.state.lock().await;
        match &*guard {
            ClientState::Ready(service) => Ok(Arc::clone(service)),
            ClientState::Connecting(_) => Err(anyhow!("MCP client not initialized")),
        }
    }
}
