use std::collections::HashMap;
use std::time::Duration;

use fabro_mcp::connection_manager::McpConnectionManager;

use fabro_mcp::client::McpClient;
use fabro_mcp::config::{McpServerConfig, McpTransport};
use fabro_mcp::connection_manager::call_result_to_string;

fn test_server_config() -> McpServerConfig {
    let test_server = format!("{}/tests/test_mcp_server.py", env!("CARGO_MANIFEST_DIR"));
    McpServerConfig {
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
async fn stdio_client_initialize_and_list_tools() {
    let config = test_server_config();
    let client = McpClient::new(&config).unwrap();
    client.initialize(config.startup_timeout()).await.unwrap();

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].0, "echo");
    assert_eq!(tools[0].1, "Echo back the message");
}

#[tokio::test]
async fn stdio_client_call_tool_echo() {
    let config = test_server_config();
    let client = McpClient::new(&config).unwrap();
    client.initialize(config.startup_timeout()).await.unwrap();

    let result = client
        .call_tool(
            "echo",
            serde_json::json!({"message": "hello from rust"}),
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    let text = call_result_to_string(&result).unwrap();
    assert_eq!(text, "hello from rust");
}

#[tokio::test]
async fn connection_manager_stdio_roundtrip() {
    let config = test_server_config();
    let mut mgr = McpConnectionManager::new();
    let results = mgr.start_servers(&[config]).await;

    assert_eq!(results.len(), 1);
    let (name, tool_count) = &results[0];
    assert_eq!(name, "test-echo");
    assert_eq!(*tool_count.as_ref().unwrap(), 1);

    let tools = mgr.all_tools();
    assert!(tools.contains_key("mcp__test_echo__echo"));

    let result = mgr
        .call_tool(
            "mcp__test_echo__echo",
            serde_json::json!({"message": "roundtrip"}),
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    let text = call_result_to_string(&result).unwrap();
    assert_eq!(text, "roundtrip");
}
