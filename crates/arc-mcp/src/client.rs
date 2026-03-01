use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::info;

use crate::client_handler::LoggingClientHandler;
use crate::config::{McpServerConfig, McpTransport};

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
    pub fn new(config: &McpServerConfig) -> Result<Self> {
        let transport = match &config.transport {
            McpTransport::Stdio { command, args, env } => {
                let mut cmd = Command::new(command);
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
                    let mut header_map = reqwest::header::HeaderMap::new();
                    for (key, value) in headers {
                        let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                            .map_err(|e| anyhow!("invalid header name '{}': {}", key, e))?;
                        let val = reqwest::header::HeaderValue::from_str(value)
                            .map_err(|e| anyhow!("invalid header value for '{}': {}", key, e))?;
                        header_map.insert(name, val);
                    }
                    builder = builder.default_headers(header_map);
                }

                let http_client = builder.build()?;
                let transport =
                    StreamableHttpClientTransport::with_client(http_client, http_config);

                PendingTransport::Http(transport)
            }
        };

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

            let handshake = async {
                match transport {
                    PendingTransport::Stdio(t) => {
                        rmcp::service::serve_client(handler.clone(), t).await
                    }
                    PendingTransport::Http(t) => {
                        rmcp::service::serve_client(handler.clone(), t).await
                    }
                }
            };

            let service = tokio::time::timeout(timeout, handshake)
                .await
                .map_err(|_| {
                    anyhow!(
                        "timed out initializing MCP server '{}' after {:?}",
                        self.server_name,
                        timeout
                    )
                })?
                .map_err(|e| {
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

        let tools = result
            .into_iter()
            .map(|tool| {
                let name = tool.name.to_string();
                let description = tool.description.as_deref().unwrap_or("").to_string();
                let input_schema = serde_json::to_value(&*tool.input_schema).unwrap_or_default();
                (name, description, input_schema)
            })
            .collect();

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
                    "MCP tool arguments must be a JSON object, got {}",
                    other
                ));
            }
        };

        let params = CallToolRequestParams {
            meta: None,
            name: name.to_string().into(),
            arguments: args,
            task: None,
        };

        let result = tokio::time::timeout(timeout, service.call_tool(params))
            .await
            .map_err(|_| {
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
