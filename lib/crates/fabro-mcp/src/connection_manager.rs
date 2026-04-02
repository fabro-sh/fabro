use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rmcp::model::{CallToolResult, RawContent};
use tracing::{error, info};

use crate::client::McpClient;
use crate::config::McpServerSettings;

const MCP_TOOL_NAME_DELIMITER: &str = "__";

/// Produce a qualified tool name: `mcp__{server}__{tool}`.
/// Non-alphanumeric characters in `server` and `tool` (except `_`) are replaced with `_`.
#[must_use]
pub fn qualified_tool_name(server: &str, tool: &str) -> String {
    format!(
        "mcp{delim}{server}{delim}{tool}",
        delim = MCP_TOOL_NAME_DELIMITER,
        server = sanitize_name(server),
        tool = sanitize_name(tool),
    )
}

/// Parse a qualified tool name back into `(server, tool)`.
/// Returns `None` if the name doesn't match the expected pattern.
#[must_use]
pub fn parse_qualified_name(qualified: &str) -> Option<(String, String)> {
    let rest = qualified.strip_prefix("mcp")?;
    let rest = rest.strip_prefix(MCP_TOOL_NAME_DELIMITER)?;
    let idx = rest.find(MCP_TOOL_NAME_DELIMITER)?;
    let server = &rest[..idx];
    let tool = &rest[idx + MCP_TOOL_NAME_DELIMITER.len()..];
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Convert an MCP `CallToolResult` to a `Result<String, String>`.
/// `Ok(text)` for success, `Err(text)` if the result has `is_error` set.
pub fn call_result_to_string(result: &CallToolResult) -> Result<String, String> {
    let text = result
        .content
        .iter()
        .map(|c| match &c.raw {
            RawContent::Text(t) => t.text.clone(),
            RawContent::Image(_) => "[image content]".to_string(),
            RawContent::Audio(_) => "[audio content]".to_string(),
            RawContent::Resource(_) | RawContent::ResourceLink(_) => {
                "[resource content]".to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if result.is_error.unwrap_or(false) {
        Err(text)
    } else {
        Ok(text)
    }
}

/// Tool info stored per-tool in the connection manager.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub server_name: String,
    pub original_tool_name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Manages connections to multiple MCP servers and their tools.
pub struct McpConnectionManager {
    clients: HashMap<String, Arc<McpClient>>,
    tools: HashMap<String, ToolInfo>,
}

impl McpConnectionManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            tools: HashMap::new(),
        }
    }

    /// Start all configured MCP servers. Failed servers are logged but don't block others.
    /// Returns a list of `(server_name, result)` for each server.
    pub async fn start_servers(
        &mut self,
        configs: &[McpServerSettings],
    ) -> Vec<(String, Result<usize>)> {
        let mut results = Vec::new();

        for config in configs {
            match self.start_one_server(config).await {
                Ok(tool_count) => {
                    info!(server = %config.name, tools = tool_count, "MCP server ready");
                    results.push((config.name.clone(), Ok(tool_count)));
                }
                Err(e) => {
                    error!(server = %config.name, error = %e, "MCP server failed to start");
                    results.push((config.name.clone(), Err(e)));
                }
            }
        }

        results
    }

    async fn start_one_server(&mut self, config: &McpServerSettings) -> Result<usize> {
        let client = McpClient::new(config)?;
        client.initialize(config.startup_timeout()).await?;
        let tools = client.list_tools().await?;
        let tool_count = tools.len();

        for (name, description, input_schema) in tools {
            let qualified = qualified_tool_name(&config.name, &name);
            self.tools.insert(
                qualified,
                ToolInfo {
                    server_name: config.name.clone(),
                    original_tool_name: name,
                    description,
                    input_schema,
                },
            );
        }

        self.clients.insert(config.name.clone(), Arc::new(client));

        Ok(tool_count)
    }

    /// All tools across all connected servers.
    #[must_use]
    pub fn all_tools(&self) -> &HashMap<String, ToolInfo> {
        &self.tools
    }

    /// Call a tool by its qualified name.
    pub async fn call_tool(
        &self,
        qualified_name: &str,
        arguments: serde_json::Value,
        timeout: Duration,
    ) -> Result<CallToolResult> {
        let info = self
            .tools
            .get(qualified_name)
            .ok_or_else(|| anyhow::anyhow!("unknown MCP tool: {qualified_name}"))?;

        let client = self
            .clients
            .get(&info.server_name)
            .ok_or_else(|| anyhow::anyhow!("no client for MCP server: {}", info.server_name))?;

        client
            .call_tool(&info.original_tool_name, arguments, timeout)
            .await
    }
}

impl Default for McpConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{Content, RawTextContent};

    #[test]
    fn qualified_tool_name_basic() {
        assert_eq!(
            qualified_tool_name("filesystem", "read_file"),
            "mcp__filesystem__read_file"
        );
    }

    #[test]
    fn qualified_tool_name_sanitizes_special_chars() {
        assert_eq!(
            qualified_tool_name("my-server", "read.file"),
            "mcp__my_server__read_file"
        );
    }

    #[test]
    fn qualified_tool_name_preserves_underscores() {
        assert_eq!(
            qualified_tool_name("my_server", "read_file"),
            "mcp__my_server__read_file"
        );
    }

    #[test]
    fn parse_qualified_name_roundtrip() {
        let qualified = qualified_tool_name("filesystem", "read_file");
        let (server, tool) = parse_qualified_name(&qualified).unwrap();
        assert_eq!(server, "filesystem");
        assert_eq!(tool, "read_file");
    }

    #[test]
    fn parse_qualified_name_with_sanitized_input() {
        let qualified = qualified_tool_name("my-server", "read.file");
        let (server, tool) = parse_qualified_name(&qualified).unwrap();
        assert_eq!(server, "my_server");
        assert_eq!(tool, "read_file");
    }

    #[test]
    fn parse_qualified_name_invalid_prefix() {
        assert!(parse_qualified_name("not_mcp__server__tool").is_none());
    }

    #[test]
    fn parse_qualified_name_missing_delimiter() {
        assert!(parse_qualified_name("mcp__serveronly").is_none());
    }

    #[test]
    fn parse_qualified_name_empty_parts() {
        assert!(parse_qualified_name("mcp____tool").is_none());
    }

    fn make_text_content(text: &str) -> Content {
        Content {
            raw: RawContent::Text(RawTextContent {
                text: text.to_string(),
                meta: None,
            }),
            annotations: None,
        }
    }

    fn make_call_result(content: Vec<Content>, is_error: Option<bool>) -> CallToolResult {
        CallToolResult {
            content,
            structured_content: None,
            is_error,
            meta: None,
        }
    }

    #[test]
    fn call_result_to_string_text_success() {
        let result = make_call_result(vec![make_text_content("hello world")], Some(false));
        assert_eq!(
            call_result_to_string(&result),
            Ok("hello world".to_string())
        );
    }

    #[test]
    fn call_result_to_string_text_error() {
        let result = make_call_result(vec![make_text_content("something failed")], Some(true));
        assert_eq!(
            call_result_to_string(&result),
            Err("something failed".to_string())
        );
    }

    #[test]
    fn call_result_to_string_multiple_blocks_concatenated() {
        let result = make_call_result(
            vec![make_text_content("line 1"), make_text_content("line 2")],
            None,
        );
        assert_eq!(
            call_result_to_string(&result),
            Ok("line 1\nline 2".to_string())
        );
    }

    #[test]
    fn call_result_to_string_image_placeholder() {
        use rmcp::model::RawImageContent;
        let result = CallToolResult {
            content: vec![Content {
                raw: RawContent::Image(RawImageContent {
                    data: "base64data".to_string(),
                    mime_type: "image/png".to_string(),
                    meta: None,
                }),
                annotations: None,
            }],
            structured_content: None,
            is_error: None,
            meta: None,
        };
        assert_eq!(
            call_result_to_string(&result),
            Ok("[image content]".to_string())
        );
    }

    #[test]
    fn call_result_to_string_none_is_error_treated_as_success() {
        let result = make_call_result(vec![make_text_content("ok")], None);
        assert_eq!(call_result_to_string(&result), Ok("ok".to_string()));
    }

    #[test]
    fn connection_manager_new_has_empty_tools() {
        let mgr = McpConnectionManager::new();
        assert!(mgr.all_tools().is_empty());
    }
}
