use std::sync::Arc;

use arc_llm::types::ToolDefinition;
use arc_mcp::connection_manager::{call_result_to_string, McpConnectionManager};

use crate::tool_registry::RegisteredTool;

/// Create `RegisteredTool` instances for every tool exposed by connected MCP servers.
pub fn make_mcp_tools(manager: Arc<McpConnectionManager>) -> Vec<RegisteredTool> {
    manager
        .all_tools()
        .iter()
        .map(|(qualified_name, info)| {
            let mgr = Arc::clone(&manager);
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
    use super::*;
    use std::collections::HashMap;

    use arc_mcp::config::{McpServerConfig, McpTransport};

    fn test_server_config() -> McpServerConfig {
        let test_server = format!(
            "{}/../arc-mcp/tests/test_mcp_server.py",
            env!("CARGO_MANIFEST_DIR")
        );
        McpServerConfig {
            name: "test-echo".into(),
            transport: McpTransport::Stdio {
                command: "python3".into(),
                args: vec![test_server],
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

        let tools = make_mcp_tools(Arc::new(mgr));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].definition.name, "mcp__test_echo__echo");
        assert_eq!(tools[0].definition.description, "Echo back the message");
    }

    #[tokio::test]
    async fn mcp_tool_executor_calls_through() {
        let config = test_server_config();
        let mut mgr = McpConnectionManager::new();
        mgr.start_servers(&[config]).await;

        let tools = make_mcp_tools(Arc::new(mgr));
        let tool = &tools[0];

        use crate::execution_env::ExecutionEnvironment;
        use crate::test_support::MockExecutionEnvironment;
        use crate::tool_registry::ToolContext;
        use tokio_util::sync::CancellationToken;

        let env: Arc<dyn ExecutionEnvironment> = Arc::new(MockExecutionEnvironment::default());
        let result = (tool.executor)(
            serde_json::json!({"message": "test message"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
            },
        )
        .await;
        assert_eq!(result.unwrap(), "test message");
    }
}
