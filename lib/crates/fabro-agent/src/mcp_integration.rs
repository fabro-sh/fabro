use std::sync::Arc;

use fabro_llm::types::ToolDefinition;
use fabro_mcp::connection_manager::{McpConnectionManager, call_result_to_string};

use crate::tool_registry::RegisteredTool;

/// Create `RegisteredTool` instances for every tool exposed by connected MCP
/// servers.
pub fn make_mcp_tools(manager: &Arc<McpConnectionManager>) -> Vec<RegisteredTool> {
    manager
        .all_tools()
        .iter()
        .map(|(qualified_name, info)| {
            let mgr = Arc::clone(manager);
            let name = qualified_name.clone();
            let tool_timeout = std::time::Duration::from_secs(120);

            RegisteredTool {
                definition: ToolDefinition {
                    name: qualified_name.clone(),
                    description: info.description.clone(),
                    parameters: info.input_schema.clone(),
                },
                executor: Arc::new(move |args, _ctx| {
                    let mgr = Arc::clone(&mgr);
                    let name = name.clone();
                    let timeout = tool_timeout;
                    Box::pin(async move {
                        let result = mgr
                            .call_tool(&name, args, timeout)
                            .await
                            .map_err(|e| e.to_string())?;
                        call_result_to_string(&result)
                    })
                }),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_mcp::config::{McpServerSettings, McpTransport};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;
    use crate::tool_registry::ToolContext;

    fn test_server_config() -> McpServerSettings {
        let test_server = format!(
            "{}/../fabro-mcp/tests/test_mcp_server.py",
            env!("CARGO_MANIFEST_DIR")
        );
        McpServerSettings {
            name: "test-echo".into(),
            transport: McpTransport::Stdio {
                command: vec!["python3".into(), test_server],
                env: HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs: 30,
        }
    }

    #[tokio::test]
    async fn make_mcp_tools_produces_registered_tools() {
        let config = test_server_config();
        let mut mgr = McpConnectionManager::new();
        mgr.start_servers(&[config]).await;

        let tools = make_mcp_tools(&Arc::new(mgr));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].definition.name, "mcp__test_echo__echo");
        assert_eq!(tools[0].definition.description, "Echo back the message");
    }

    #[tokio::test]
    async fn mcp_tool_executor_calls_through() {
        let config = test_server_config();
        let mut mgr = McpConnectionManager::new();
        mgr.start_servers(&[config]).await;

        let tools = make_mcp_tools(&Arc::new(mgr));
        let tool = &tools[0];

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = (tool.executor)(
            serde_json::json!({"message": "test message"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "test message");
    }
}
