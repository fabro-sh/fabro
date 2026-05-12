use std::collections::HashMap;
use std::time::Duration;

use fabro_mcp::client::McpClient;
use fabro_mcp::config::{McpServerSettings, McpTransport};
use fabro_mcp::connection_manager::{McpConnectionManager, call_result_to_string};

fn test_server_config() -> McpServerSettings {
    let test_server = format!("{}/tests/test_mcp_server.py", env!("CARGO_MANIFEST_DIR"));
    McpServerSettings {
        name:                 "test-echo".into(),
        transport:            McpTransport::Stdio {
            command: vec!["python3".into(), test_server],
            env:     HashMap::new(),
        },
        current_dir:          None,
        clear_env:            false,
        startup_timeout_secs: 10,
        tool_timeout_secs:    30,
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
#[expect(
    clippy::disallowed_methods,
    reason = "stdio integration test stages a local process cwd and inherits PATH for python3 lookup"
)]
async fn stdio_client_uses_configured_cwd_and_exact_env() {
    let test_server = format!("{}/tests/test_mcp_server.py", env!("CARGO_MANIFEST_DIR"));
    let temp_dir = std::env::temp_dir().join(format!(
        "fabro-mcp-stdio-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&temp_dir).unwrap();
    let canonical_temp_dir = std::fs::canonicalize(&temp_dir).unwrap();
    let mut env = HashMap::new();
    env.insert(
        "PATH".to_string(),
        std::env::var("PATH").expect("PATH should be set for python3 lookup"),
    );
    env.insert("FABRO_MCP_TEST_SENTINEL".to_string(), "fixture".to_string());
    let config = McpServerSettings {
        name:                 "test-echo".into(),
        transport:            McpTransport::Stdio {
            command: vec!["python3".into(), test_server],
            env,
        },
        current_dir:          Some(canonical_temp_dir.clone()),
        clear_env:            true,
        startup_timeout_secs: 10,
        tool_timeout_secs:    30,
    };
    let client = McpClient::new(&config).unwrap();
    client.initialize(config.startup_timeout()).await.unwrap();

    let cwd = client
        .call_tool(
            "echo",
            serde_json::json!({"message": "__cwd__"}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(
        call_result_to_string(&cwd).unwrap(),
        canonical_temp_dir.display().to_string()
    );
    let sentinel = client
        .call_tool(
            "echo",
            serde_json::json!({"message": "__env:FABRO_MCP_TEST_SENTINEL__"}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(call_result_to_string(&sentinel).unwrap(), "fixture");
    let home = client
        .call_tool(
            "echo",
            serde_json::json!({"message": "__env:HOME__"}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(call_result_to_string(&home).unwrap(), "");

    client.shutdown().await.unwrap();
    std::fs::remove_dir(&temp_dir).unwrap();
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
