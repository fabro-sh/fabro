#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage MCP config files with sync std::fs"
)]
#![expect(
    clippy::disallowed_types,
    reason = "raw stdio regression test intentionally uses blocking std pipes outside Tokio"
)]

use std::collections::HashMap;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use fabro_client::{AuthEntry, AuthStore, DevTokenEntry, OAuthEntry, StoredSubject};
use fabro_mcp::client::McpClient;
use fabro_mcp::config::{McpServerSettings, McpTransport};
use fabro_test::{fabro_json_snapshot, fabro_snapshot, test_context};
use fabro_types::RunId;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;

use super::support::{mock_resolved_run, remote_run_summary_json};
use crate::support::{
    RealAuthHarness, TEST_DEV_TOKEN, run_projection_json, seed_dev_token_auth, unique_run_id,
};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["mcp", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Model Context Protocol server

    Usage: fabro mcp [OPTIONS] <COMMAND>

    Commands:
      start   Start the Fabro MCP server over stdio
      config  Print MCP client configuration JSON
      init    Configure an MCP client to launch Fabro
      help    Print this message or the help of the given subcommand(s)

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn start_help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["mcp", "start", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Start the Fabro MCP server over stdio

    Usage: fabro mcp start [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn config_help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["mcp", "config", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Print MCP client configuration JSON

    Usage: fabro mcp config [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn init_help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["mcp", "init", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Configure an MCP client to launch Fabro

    Usage: fabro mcp init [OPTIONS] <AGENT>

    Arguments:
      <AGENT>  [possible values: claude, cursor, windsurf]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn config_prints_generic_mcp_json() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["mcp", "config"]);
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {
      "mcpServers": {
        "fabro": {
          "command": "fabro",
          "args": [
            "mcp",
            "start"
          ]
        }
      }
    }
    ----- stderr -----
    "#);
}

#[test]
fn config_preserves_connection_flags() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args([
        "mcp",
        "config",
        "--server",
        "https://example.test/api/v1",
        "--storage-dir",
        "/tmp/fabro-mcp-storage",
    ]);
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {
      "mcpServers": {
        "fabro": {
          "command": "fabro",
          "args": [
            "mcp",
            "start",
            "--server",
            "https://example.test/api/v1",
            "--storage-dir",
            "/tmp/fabro-mcp-storage"
          ]
        }
      }
    }
    ----- stderr -----
    "#);
}

#[test]
fn init_cursor_writes_idempotent_config() {
    let context = test_context!();
    context
        .command()
        .args(["mcp", "init", "cursor"])
        .assert()
        .success();
    context
        .command()
        .args(["mcp", "init", "cursor"])
        .assert()
        .success();

    let config_path = context.home_dir.join(".cursor").join("mcp.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    fabro_json_snapshot!(context, config, @r#"
    {
      "mcpServers": {
        "fabro": {
          "command": "fabro",
          "args": [
            "mcp",
            "start"
          ]
        }
      }
    }
    "#);
}

#[test]
fn init_claude_writes_desktop_and_code_configs() {
    let context = test_context!();
    context
        .command()
        .args(["mcp", "init", "claude"])
        .assert()
        .success();

    let desktop_config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(expected_claude_desktop_config_path(&context.home_dir)).unwrap(),
    )
    .unwrap();
    fabro_json_snapshot!(context, desktop_config, @r#"
    {
      "mcpServers": {
        "fabro": {
          "command": "fabro",
          "args": [
            "mcp",
            "start"
          ]
        }
      }
    }
    "#);

    let code_config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(context.home_dir.join(".claude.json")).unwrap(),
    )
    .unwrap();
    fabro_json_snapshot!(context, code_config, @r#"
    {
      "mcpServers": {
        "fabro": {
          "command": "fabro",
          "args": [
            "mcp",
            "start"
          ]
        }
      }
    }
    "#);
}

#[test]
fn init_claude_preserves_existing_claude_code_config() {
    let context = test_context!();
    let claude_code_path = context.home_dir.join(".claude.json");
    std::fs::write(
        &claude_code_path,
        r#"{"numStartups":42,"mcpServers":{"other":{"type":"http","url":"https://example.test/mcp"}}}"#,
    )
    .unwrap();

    context
        .command()
        .args(["mcp", "init", "claude"])
        .assert()
        .success();

    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_code_path).unwrap()).unwrap();
    fabro_json_snapshot!(context, config, @r#"
    {
      "numStartups": 42,
      "mcpServers": {
        "other": {
          "type": "http",
          "url": "https://example.test/mcp"
        },
        "fabro": {
          "command": "fabro",
          "args": [
            "mcp",
            "start"
          ]
        }
      }
    }
    "#);
}

#[test]
fn init_windsurf_writes_config() {
    let context = test_context!();
    context
        .command()
        .args(["mcp", "init", "windsurf"])
        .assert()
        .success();

    let config_path = context
        .home_dir
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    fabro_json_snapshot!(context, config, @r#"
    {
      "mcpServers": {
        "fabro": {
          "command": "fabro",
          "args": [
            "mcp",
            "start"
          ]
        }
      }
    }
    "#);
}

#[test]
fn init_preserves_existing_servers() {
    let context = test_context!();
    let config_path = context.home_dir.join(".cursor").join("mcp.json");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        r#"{"mcpServers":{"other":{"command":"other","args":["serve"]}},"theme":"dark"}"#,
    )
    .unwrap();

    context
        .command()
        .args([
            "mcp",
            "init",
            "cursor",
            "--server",
            "https://example.test/api/v1",
        ])
        .assert()
        .success();

    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    fabro_json_snapshot!(context, config, @r#"
    {
      "mcpServers": {
        "other": {
          "command": "other",
          "args": [
            "serve"
          ]
        },
        "fabro": {
          "command": "fabro",
          "args": [
            "mcp",
            "start",
            "--server",
            "https://example.test/api/v1"
          ]
        }
      },
      "theme": "dark"
    }
    "#);
}

#[test]
fn init_invalid_json_fails_without_overwrite() {
    let context = test_context!();
    let config_path = context.home_dir.join(".cursor").join("mcp.json");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "{not json").unwrap();

    let mut cmd = context.command();
    cmd.args(["mcp", "init", "cursor"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × failed to parse MCP config [HOME_DIR]/.cursor/mcp.json
      ╰─▶ key must be a string at line 1 column 2
    ");
    assert_eq!(std::fs::read_to_string(config_path).unwrap(), "{not json");
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_server_initializes_and_lists_run_tools() {
    let context = test_context!();
    let client = spawn_mcp_client(&context, &[]).await;

    let tools = client.list_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(names, vec![
        "fabro_run_create",
        "fabro_run_events",
        "fabro_run_gather",
        "fabro_run_interact",
        "fabro_run_pair",
        "fabro_run_search",
    ]);
    for (name, _, schema) in &tools {
        assert!(
            schema.is_object(),
            "tool should have input schema: {schema}"
        );
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("tool input schema should have properties");
        for (property, property_schema) in properties {
            assert!(
                property_schema.is_object(),
                "{name}.{property} should use an object JSON Schema, got {property_schema}"
            );
        }
    }
    let interact_schema = tools
        .iter()
        .find(|(name, _, _)| name == "fabro_run_interact")
        .map(|(_, _, schema)| schema)
        .expect("fabro_run_interact tool should be listed");
    assert!(
        interact_schema
            .pointer("/properties/answer")
            .is_some_and(serde_json::Value::is_object),
        "fabro_run_interact.answer should have an object JSON Schema: {interact_schema}"
    );
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[test]
fn stdio_start_writes_only_json_rpc_to_stdout() {
    let context = test_context!();
    let fixture = mcp_stdio_fixture(&context, &[]);
    let mut cmd = std::process::Command::new(&fixture.command[0]);
    cmd.args(&fixture.command[1..])
        .env_clear()
        .envs(&fixture.env)
        .current_dir(&fixture.current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"fabro-test","version":"0.0.0"}}}}}}"#
    )
    .unwrap();

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(result.map(|_| line));
    });

    let line = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("initialize response should arrive")
        .expect("stdout should be readable");
    let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(value["jsonrpc"], "2.0");

    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_startup_and_list_tools_is_fast() {
    let context = test_context!();
    let start = std::time::Instant::now();
    let client = spawn_mcp_client(&context, &[]).await;
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 6);
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_create_and_search_manage_real_runs_with_cli_auth() {
    let context = test_context!();
    let harness =
        RealAuthHarness::start_with_dev_token(fabro_test::GitHubAppState::default()).await;
    let target_url = harness.api_target();
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let workflow = context.install_fixture("simple.fabro");

    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;

    let create = call_tool_json(
        &client,
        "fabro_run_create",
        serde_json::json!({
            "runs": [{
                "workflow": workflow,
                "dry_run": true,
                "auto_approve": true,
                "labels": { "source": "mcp-test" }
            }]
        }),
    )
    .await;
    let run_id = create["runs"][0]["run_id"].as_str().unwrap().to_string();
    assert_eq!(create["runs"][0]["started"], true);

    let search = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({
            "run_ids": [run_id],
            "labels": { "source": "mcp-test" },
            "first": 10
        }),
    )
    .await;
    fabro_json_snapshot!(context, normalize_run_search(search), @r#"
    {
      "runs": [
        {
          "run_id": "[RUN_ID]",
          "parent_id": null,
          "children_count": 0,
          "workflow_name": null,
          "workflow_graph_name": "Simple",
          "workflow_slug": "simple",
          "status": "queued",
          "archived": false,
          "created_at": "[TIMESTAMP]",
          "started_at": null,
          "completed_at": null,
          "labels": {
            "source": "mcp-test"
          },
          "source_directory": "[SOURCE_DIRECTORY]",
          "repo_origin_url": null,
          "goal_preview": "Run tests and report results",
          "goal_truncated": false
        }
      ],
      "next_cursor": null
    }
    "#);

    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_run_tools_use_default_local_server_without_server_flag() {
    let context = test_context!();
    let workflow = context.install_fixture("simple.fabro");
    let client = spawn_mcp_client(&context, &[]).await;

    let create = call_tool_json(
        &client,
        "fabro_run_create",
        serde_json::json!({
            "runs": [{
                "workflow": workflow,
                "dry_run": true,
                "auto_approve": true,
                "labels": { "source": "mcp-default-server-test" },
                "start": false
            }]
        }),
    )
    .await;
    let run_id = create["runs"][0]["run_id"].as_str().unwrap();
    let search = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [run_id], "first": 1 }),
    )
    .await;

    assert_eq!(search["runs"][0]["run_id"], run_id);
    assert_eq!(
        search["runs"][0]["labels"]["source"],
        "mcp-default-server-test"
    );
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_configured_unix_target_auto_spawns_like_cli() {
    let mut context = test_context!();
    let isolated = fabro_test::isolated_storage_dir();
    let storage_dir = isolated.path().join("storage");
    let socket_path = isolated.path().join("configured.sock");
    write_mcp_server_settings(&mut context, &storage_dir, Some(&socket_path));
    let workflow = context.install_fixture("simple.fabro");

    assert!(!socket_path.exists());
    let client = spawn_mcp_client(&context, &[]).await;
    let run_id = create_mcp_run(&client, workflow, false).await;
    let search = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [run_id], "first": 1 }),
    )
    .await;

    assert_eq!(search["runs"][0]["run_id"], run_id);
    assert!(
        socket_path.exists(),
        "configured Unix socket should be auto-spawned"
    );
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_explicit_unix_target_auto_spawns_like_cli() {
    let mut context = test_context!();
    let isolated = fabro_test::isolated_storage_dir();
    let storage_dir = isolated.path().join("storage");
    let socket_path = isolated.path().join("explicit.sock");
    write_mcp_server_settings(&mut context, &storage_dir, None);
    let workflow = context.install_fixture("simple.fabro");
    let storage_arg = storage_dir.display().to_string();
    let socket_arg = socket_path.display().to_string();

    assert!(!socket_path.exists());
    let client = spawn_mcp_client(&context, &[
        "--storage-dir",
        &storage_arg,
        "--server",
        &socket_arg,
    ])
    .await;
    let run_id = create_mcp_run(&client, workflow, false).await;
    let search = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [run_id], "first": 1 }),
    )
    .await;

    assert_eq!(search["runs"][0]["run_id"], run_id);
    assert!(
        socket_path.exists(),
        "explicit Unix socket should be auto-spawned"
    );
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_missing_default_settings_reports_configure_first_error_and_stays_alive() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut env = fabro_test::isolated_env(home.path());
    env.insert(
        "FABRO_HOME".to_string(),
        home.path().join(".fabro").display().to_string(),
    );
    let client = spawn_mcp_client_from_fixture(McpStdioFixture {
        command: vec![
            env!("CARGO_BIN_EXE_fabro").to_string(),
            "mcp".to_string(),
            "start".to_string(),
        ],
        env,
        current_dir: workspace.path().to_path_buf(),
    })
    .await;

    let error = call_tool_error_text(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": ["missing"], "first": 1 }),
    )
    .await;

    assert!(
        error.contains("Cannot reach Fabro server: no settings.toml configured."),
        "{error}"
    );
    assert_eq!(client.list_tools().await.unwrap().len(), 6);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_search_filters_status_dates_and_paginates() {
    let context = test_context!();
    let harness =
        RealAuthHarness::start_with_dev_token(fabro_test::GitHubAppState::default()).await;
    let target_url = harness.api_target();
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let workflow = context.install_fixture("simple.fabro");
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;
    let first = create_mcp_run(&client, workflow.clone(), false).await;
    let second = create_mcp_run(&client, workflow, false).await;

    let page_one = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({
            "labels": { "source": "mcp-test" },
            "status": ["submitted"],
            "archived": false,
            "created_after": "2000-01-01",
            "created_before": "2100-01-01T00:00:00Z",
            "first": 1
        }),
    )
    .await;
    let cursor = page_one["next_cursor"]
        .as_str()
        .expect("first page should have cursor");
    let page_two = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({
            "labels": { "source": "mcp-test" },
            "status": ["submitted"],
            "archived": false,
            "after": cursor,
            "first": 1
        }),
    )
    .await;

    let page_one_id = page_one["runs"][0]["run_id"].as_str().unwrap();
    let page_two_id = page_two["runs"][0]["run_id"].as_str().unwrap();
    assert_ne!(page_one_id, page_two_id);
    assert!([first.as_str(), second.as_str()].contains(&page_one_id));
    assert!([first.as_str(), second.as_str()].contains(&page_two_id));

    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_search_hides_archived_runs_by_default() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let active_id = unique_run_id();
    let archived_id = unique_run_id();
    let active = remote_run_summary_json(
        &active_id,
        "Simple",
        "simple",
        "Active run",
        &serde_json::json!({ "kind": "succeeded", "reason": "completed" }),
        "2026-04-05T12:00:00Z",
    );
    let mut archived = remote_run_summary_json(
        &archived_id,
        "Simple",
        "simple",
        "Archived run",
        &serde_json::json!({ "kind": "succeeded", "reason": "completed" }),
        "2026-04-05T12:01:00Z",
    );
    archived["lifecycle"]["archived"] = serde_json::json!(true);
    archived["lifecycle"]["archived_at"] = serde_json::json!("2026-04-05T12:02:00Z");
    let active_resolve = mock_resolved_run_json(&server, &active_id, active, None);
    let archived_resolve = mock_resolved_run_json(&server, &archived_id, archived, None);

    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;
    let result = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [active_id, archived_id], "first": 10 }),
    )
    .await;

    assert_eq!(result["runs"].as_array().unwrap().len(), 1);
    assert!(
        result["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|run| run["archived"] == false)
    );
    active_resolve.assert();
    archived_resolve.assert();
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_search_refreshes_expired_oauth_token() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_oauth_auth(
        &context.home_dir,
        &target,
        "expired-access",
        "refresh-octocat",
    );
    let run_id = unique_run_id();
    let expired_access = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/runs/resolve")
            .query_param("selector", run_id.clone())
            .header("authorization", "Bearer expired-access");
        then.status(401)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "errors": [{
                    "detail": "access token expired",
                    "code": "access_token_expired"
                }]
            }));
    });
    let refresh = server.mock(|when, then| {
        when.method(POST)
            .path("/auth/cli/refresh")
            .header("authorization", "Bearer refresh-octocat");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "access_token": "fresh-access",
                "access_token_expires_at": (Utc::now() + ChronoDuration::minutes(10)).to_rfc3339(),
                "refresh_token": "fresh-refresh",
                "refresh_token_expires_at": (Utc::now() + ChronoDuration::days(30)).to_rfc3339(),
                "subject": {
                    "idp_issuer": "https://github.com",
                    "idp_subject": "12345",
                    "login": "octocat",
                    "name": "The Octocat",
                    "email": "octocat@example.com"
                }
            }));
    });
    let fresh_access = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/runs/resolve")
            .header("authorization", "Bearer fresh-access")
            .query_param("selector", run_id.clone());
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Simple",
                "simple",
                "OAuth refreshed",
                &serde_json::json!({ "kind": "submitted" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;

    let result = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [run_id], "first": 1 }),
    )
    .await;

    assert_eq!(result["runs"][0]["run_id"], run_id);
    expired_access.assert();
    refresh.assert();
    fresh_access.assert();
    let stored = AuthStore::new(context.home_dir.join(".fabro/auth.json"))
        .get(&target)
        .unwrap()
        .unwrap();
    let AuthEntry::OAuth(stored) = stored else {
        panic!("expected refreshed OAuth entry");
    };
    assert_eq!(stored.access_token, "fresh-access");
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_search_uses_fabro_auth_file_override() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    let auth_file = context.temp_dir.join("custom-auth.json");
    AuthStore::new(auth_file.clone())
        .put(
            &target,
            AuthEntry::DevToken(DevTokenEntry {
                token:        TEST_DEV_TOKEN.to_string(),
                logged_in_at: Utc::now(),
            }),
        )
        .expect("custom auth store should be seeded");
    let run_id = unique_run_id();
    let authorization = format!("Bearer {TEST_DEV_TOKEN}");
    let resolve = mock_resolved_run_json(
        &server,
        &run_id,
        remote_run_summary_json(
            &run_id,
            "Simple",
            "simple",
            "Custom auth file",
            &serde_json::json!({ "kind": "submitted" }),
            "2026-04-05T12:00:00Z",
        ),
        Some(&authorization),
    );
    let mut fixture = mcp_stdio_fixture(&context, &["--server", &target_url]);
    fixture.env.insert(
        "FABRO_AUTH_FILE".to_string(),
        auth_file.display().to_string(),
    );
    let client = spawn_mcp_client_from_fixture(fixture).await;

    let result = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [run_id], "first": 1 }),
    )
    .await;

    assert_eq!(result["runs"][0]["run_id"], run_id);
    resolve.assert();
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_search_orders_by_started_timestamp_before_created_timestamp() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let submitted_id = unique_run_id();
    let running_id = unique_run_id();
    let submitted = remote_run_summary_json(
        &submitted_id,
        "Simple",
        "simple",
        "Submitted later",
        &serde_json::json!({ "kind": "submitted" }),
        "2026-04-05T12:10:00Z",
    );
    let mut running = remote_run_summary_json(
        &running_id,
        "Simple",
        "simple",
        "Started later",
        &serde_json::json!({ "kind": "running" }),
        "2026-04-05T12:00:00Z",
    );
    running["timestamps"]["started_at"] = serde_json::json!("2026-04-05T12:20:00Z");
    let submitted_resolve = mock_resolved_run_json(&server, &submitted_id, submitted, None);
    let running_resolve = mock_resolved_run_json(&server, &running_id, running, None);

    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;
    let result = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [submitted_id, running_id], "first": 2 }),
    )
    .await;

    assert_eq!(result["runs"][0]["run_id"], running_id);
    assert_eq!(result["runs"][1]["run_id"], submitted_id);
    submitted_resolve.assert();
    running_resolve.assert();
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_search_orders_submitted_runs_by_created_timestamp_not_run_id_timestamp() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let newer_created_id = run_id_with_timestamp("2026-04-05T12:00:00Z", 1);
    let older_created_id = run_id_with_timestamp("2026-04-05T12:40:00Z", 1);
    let mut newer_created = remote_run_summary_json(
        &newer_created_id,
        "Simple",
        "simple",
        "Created later",
        &serde_json::json!({ "kind": "submitted" }),
        "2026-04-05T12:30:00Z",
    );
    newer_created["timestamps"]["started_at"] = serde_json::Value::Null;
    let mut older_created = remote_run_summary_json(
        &older_created_id,
        "Simple",
        "simple",
        "Created earlier",
        &serde_json::json!({ "kind": "submitted" }),
        "2026-04-05T12:10:00Z",
    );
    older_created["timestamps"]["started_at"] = serde_json::Value::Null;
    let newer_resolve = mock_resolved_run_json(&server, &newer_created_id, newer_created, None);
    let older_resolve = mock_resolved_run_json(&server, &older_created_id, older_created, None);

    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;
    let result = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [newer_created_id, older_created_id], "first": 2 }),
    )
    .await;

    assert_eq!(result["runs"][0]["run_id"], newer_created_id);
    assert_eq!(result["runs"][1]["run_id"], older_created_id);
    newer_resolve.assert();
    older_resolve.assert();
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_lifecycle_tools_manage_real_run() {
    let context = test_context!();
    let harness =
        RealAuthHarness::start_with_dev_token(fabro_test::GitHubAppState::default()).await;
    let target_url = harness.api_target();
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let workflow = context.install_fixture("simple.fabro");
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;
    let run_id = create_mcp_run(&client, workflow, true).await;
    let cancel = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": run_id, "action": "cancel" }),
    )
    .await;

    let gather = call_tool_json(
        &client,
        "fabro_run_gather",
        serde_json::json!({
            "run_ids": [run_id],
            "timeout_seconds": 20,
            "poll_interval_seconds": 5
        }),
    )
    .await;
    let run_id = gather["runs"][0]["run_id"].as_str().unwrap().to_string();
    let get = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": run_id, "action": "get" }),
    )
    .await;
    let events = call_tool_json(
        &client,
        "fabro_run_events",
        serde_json::json!({ "run_id": run_id, "action": "list", "first": 5 }),
    )
    .await;
    let archive = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": run_id, "action": "archive" }),
    )
    .await;
    let archived_search = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [run_id], "archived": true }),
    )
    .await;
    let unarchive = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": run_id, "action": "unarchive" }),
    )
    .await;
    let search = call_tool_json(
        &client,
        "fabro_run_search",
        serde_json::json!({ "run_ids": [run_id], "archived": false }),
    )
    .await;

    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "gather": normalize_gather(gather),
            "cancel_action": cancel["action"],
            "get_status": get["result"]["summary"]["status"],
            "events_nonempty": events["events"].as_array().is_some_and(|events| !events.is_empty()),
            "archive_action": archive["action"],
            "archived_search_count": archived_search["runs"].as_array().unwrap().len(),
            "archived_search_archived": archived_search["runs"][0]["archived"],
            "unarchive_action": unarchive["action"],
            "unarchived_search_count": search["runs"].as_array().unwrap().len(),
        }),
        @r#"
    {
      "gather": {
        "runs": [
          {
            "run_id": "[RUN_ID]",
            "parent_id": null,
            "children_count": 0,
            "workflow_name": null,
            "workflow_graph_name": "Simple",
            "workflow_slug": "simple",
            "status": "failed",
            "archived": false,
            "created_at": "[TIMESTAMP]",
            "started_at": null,
            "completed_at": "[TIMESTAMP]",
            "labels": {
              "source": "mcp-test"
            },
            "source_directory": "[SOURCE_DIRECTORY]",
            "repo_origin_url": null,
            "goal": "Run tests and report results"
          }
        ],
        "timed_out": false,
        "elapsed_seconds": "[ELAPSED]"
      },
      "cancel_action": "cancel",
      "get_status": "failed",
      "events_nonempty": true,
      "archive_action": "archive",
      "archived_search_count": 1,
      "archived_search_archived": true,
      "unarchive_action": "unarchive",
      "unarchived_search_count": 1
    }
    "#
    );

    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_gather_rejects_too_many_runs() {
    let context = test_context!();
    let client = spawn_mcp_client(&context, &["--server", "http://127.0.0.1:9"]).await;
    let run_ids = (0..51)
        .map(|index| format!("run_{index}"))
        .collect::<Vec<_>>();

    let error = call_tool_error_text(
        &client,
        "fabro_run_gather",
        serde_json::json!({ "run_ids": run_ids }),
    )
    .await;

    assert!(error.contains("run_ids"), "{error}");
    assert_eq!(client.list_tools().await.unwrap().len(), 6);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_gather_rejects_invalid_timeout_values_before_auth() {
    let context = test_context!();
    let client = spawn_mcp_client(&context, &["--server", "http://127.0.0.1:9"]).await;

    let timeout_error = call_tool_error_text(
        &client,
        "fabro_run_gather",
        serde_json::json!({
            "run_ids": ["run_123"],
            "timeout_seconds": 601,
            "poll_interval_seconds": 5
        }),
    )
    .await;
    let poll_error = call_tool_error_text(
        &client,
        "fabro_run_gather",
        serde_json::json!({
            "run_ids": ["run_123"],
            "timeout_seconds": 300,
            "poll_interval_seconds": 4
        }),
    )
    .await;

    assert!(timeout_error.contains("timeout_seconds"), "{timeout_error}");
    assert!(
        !timeout_error.contains("fabro auth login"),
        "{timeout_error}"
    );
    assert!(poll_error.contains("poll_interval_seconds"), "{poll_error}");
    assert!(!poll_error.contains("fabro auth login"), "{poll_error}");
    assert_eq!(client.list_tools().await.unwrap().len(), 6);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_gather_returns_timeout_result() {
    let context = test_context!();
    let harness =
        RealAuthHarness::start_with_dev_token(fabro_test::GitHubAppState::default()).await;
    let target_url = harness.api_target();
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let workflow = context.install_fixture("simple.fabro");
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;
    let run_id = create_mcp_run(&client, workflow, false).await;

    let start = std::time::Instant::now();
    let gather = call_tool_json(
        &client,
        "fabro_run_gather",
        serde_json::json!({
            "run_ids": [run_id],
            "timeout_seconds": 1,
            "poll_interval_seconds": 5
        }),
    )
    .await;

    assert_eq!(gather["timed_out"], true);
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
    assert_eq!(gather["runs"][0]["status"], "submitted");

    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_interact_error_does_not_stop_server() {
    let context = test_context!();
    let client = spawn_mcp_client(&context, &["--server", "http://127.0.0.1:9"]).await;

    let error = call_tool_error_text(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": "run_123", "action": "message" }),
    )
    .await;

    assert!(error.contains("message"), "{error}");
    assert_eq!(client.list_tools().await.unwrap().len(), 6);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_interact_actions_resolve_selector_and_call_expected_endpoints() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let run_id = unique_run_id();
    let selector = "nightly";
    let resolve = mock_resolved_run(&server, selector, &run_id);
    let retrieve = server.mock(|when, then| {
        when.method(GET).path(format!("/api/v1/runs/{run_id}"));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Simple",
                "simple",
                "Run tests",
                &serde_json::json!({ "kind": "running" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    let projection = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/state"));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(run_projection_json(
                &run_id,
                &serde_json::json!({ "kind": "running" }),
            ));
    });
    let start = server.mock(|when, then| {
        when.method(POST)
            .path(format!("/api/v1/runs/{run_id}/start"))
            .json_body(serde_json::json!({ "resume": false }));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Simple",
                "simple",
                "Run tests",
                &serde_json::json!({ "kind": "running" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    let message = server.mock(|when, then| {
        when.method(POST)
            .path(format!("/api/v1/runs/{run_id}/steer"))
            .json_body(serde_json::json!({ "text": "continue", "interrupt": true }));
        then.status(202);
    });
    let interrupt = server.mock(|when, then| {
        when.method(POST)
            .path(format!("/api/v1/runs/{run_id}/interrupt"));
        then.status(202);
    });
    let cancel = server.mock(|when, then| {
        when.method(POST)
            .path(format!("/api/v1/runs/{run_id}/cancel"));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Simple",
                "simple",
                "Run tests",
                &serde_json::json!({ "kind": "running" }),
                "2026-04-05T12:00:00Z",
            ));
    });

    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;
    let get = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": selector, "action": "get" }),
    )
    .await;
    let start_result = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": selector, "action": "start" }),
    )
    .await;
    let message_result = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({
            "run_id": selector,
            "action": "message",
            "message": "continue",
            "interrupt": true
        }),
    )
    .await;
    let interrupt_result = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": selector, "action": "interrupt" }),
    )
    .await;
    let cancel_result = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": selector, "action": "cancel" }),
    )
    .await;

    assert_eq!(get["result"]["summary"]["run_id"], run_id);
    assert_eq!(start_result["result"]["summary"]["run_id"], run_id);
    assert_eq!(message_result["result"]["message"], "continue");
    assert_eq!(message_result["result"]["interrupt"], true);
    assert_eq!(interrupt_result["result"]["interrupted"], true);
    assert_eq!(cancel_result["result"]["summary"]["run_id"], run_id);
    resolve.assert_calls(5);
    retrieve.assert_calls(1);
    projection.assert();
    start.assert();
    message.assert();
    interrupt.assert();
    cancel.assert();
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_create_validation_errors_happen_before_auth_or_network() {
    let context = test_context!();
    let client = spawn_mcp_client(&context, &["--server", "http://127.0.0.1:9"]).await;
    let too_many = (0..51)
        .map(|index| serde_json::json!({ "workflow": format!("wf-{index}.fabro") }))
        .collect::<Vec<_>>();

    let empty = call_tool_error_text(
        &client,
        "fabro_run_create",
        serde_json::json!({ "runs": [] }),
    )
    .await;
    let many = call_tool_error_text(
        &client,
        "fabro_run_create",
        serde_json::json!({ "runs": too_many }),
    )
    .await;
    let null = call_tool_error_text(
        &client,
        "fabro_run_create",
        serde_json::json!({
            "runs": [{
                "workflow": "simple.fabro",
                "inputs": { "decision": null }
            }]
        }),
    )
    .await;

    assert!(empty.contains("runs"), "{empty}");
    assert!(many.contains("runs"), "{many}");
    assert!(null.contains("decision"), "{null}");
    assert_eq!(client.list_tools().await.unwrap().len(), 6);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_interact_answer_validation_happens_before_auth_or_network() {
    let context = test_context!();
    let client = spawn_mcp_client(&context, &["--server", "http://127.0.0.1:9"]).await;

    let error = call_tool_error_text(
        &client,
        "fabro_run_interact",
        serde_json::json!({
            "run_id": "nightly",
            "action": "answer",
            "question_id": "q-1",
            "answer": { "value": "yes" }
        }),
    )
    .await;

    assert!(error.contains("option, options, text"), "{error}");
    assert_eq!(client.list_tools().await.unwrap().len(), 6);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_interact_questions_and_answers_use_api_wire_contract() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let run_id = unique_run_id();
    let selector = "nightly";
    let resolve = mock_resolved_run(&server, selector, &run_id);
    let questions = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/questions"))
            .query_param("page[limit]", "100")
            .query_param("page[offset]", "0");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": [{
                    "id": "q-1",
                    "text": "Proceed?",
                    "stage": "gate",
                    "question_type": "yes_no",
                    "options": [],
                    "allow_freeform": false,
                    "timeout_seconds": null,
                    "context_display": null
                }],
                "meta": { "has_more": false }
            }));
    });
    let expected_answers = [
        (
            serde_json::json!(true),
            serde_json::json!({ "kind": "yes" }),
        ),
        (
            serde_json::json!(false),
            serde_json::json!({ "kind": "no" }),
        ),
        (
            serde_json::json!("Looks good"),
            serde_json::json!({ "kind": "text", "text": "Looks good" }),
        ),
        (
            serde_json::json!({ "option": "approve" }),
            serde_json::json!({ "kind": "selected", "option_key": "approve" }),
        ),
        (
            serde_json::json!({ "options": ["approve", "notify"] }),
            serde_json::json!({ "kind": "multi_selected", "option_keys": ["approve", "notify"] }),
        ),
        (
            serde_json::json!({ "text": "Freeform" }),
            serde_json::json!({ "kind": "text", "text": "Freeform" }),
        ),
    ];
    let answer_mocks = expected_answers
        .iter()
        .map(|(_, expected_body)| {
            server.mock(|when, then| {
                when.method(POST)
                    .path(format!("/api/v1/runs/{run_id}/questions/q-1/answer"))
                    .json_body(expected_body.clone());
                then.status(204);
            })
        })
        .collect::<Vec<_>>();

    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;
    let question_result = call_tool_json(
        &client,
        "fabro_run_interact",
        serde_json::json!({ "run_id": selector, "action": "get_questions" }),
    )
    .await;
    assert_eq!(question_result["result"]["questions"][0]["id"], "q-1");

    for (answer, _) in expected_answers {
        let result = call_tool_json(
            &client,
            "fabro_run_interact",
            serde_json::json!({
                "run_id": selector,
                "action": "answer",
                "question_id": "q-1",
                "answer": answer
            }),
        )
        .await;
        assert_eq!(result["result"]["submitted"], true);
    }

    resolve.assert_calls(7);
    questions.assert();
    for answer in answer_mocks {
        answer.assert();
    }
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_events_filters_find_matches_beyond_first_page() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let run_id = unique_run_id();
    let resolve = mock_resolved_run(&server, "nightly", &run_id);
    let events = (1..=60)
        .map(|sequence| {
            let event_name = if sequence == 60 {
                "stage.started"
            } else {
                "run.started"
            };
            let properties = if sequence == 60 {
                serde_json::json!({
                    "index": 1,
                    "handler_type": "prompt",
                    "attempt": 1,
                    "max_attempts": 1
                })
            } else {
                serde_json::json!({
                    "name": "Simple",
                    "goal": format!("ordinary event {sequence}")
                })
            };
            serde_json::json!({
                "seq": sequence,
                "id": format!("evt-{sequence}"),
                "ts": "2026-04-05T12:00:00Z",
                "run_id": run_id,
                "event": event_name,
                "properties": properties,
                "actor": null
            })
        })
        .collect::<Vec<_>>();
    let first_event = events[0].clone();
    let _limited_events = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param("limit", "1");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": [first_event],
                "meta": { "has_more": true }
            }));
    });
    let list_events = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param_missing("limit");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": events,
                "meta": { "has_more": false }
            }));
    });
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;

    let details = call_tool_json(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "nightly",
            "action": "details",
            "event_ids": ["evt-60"],
            "first": 1
        }),
    )
    .await;
    let filtered = call_tool_json(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "nightly",
            "action": "search",
            "categories": ["stage"],
            "query": "prompt",
            "first": 1,
            "max_content_length": 32
        }),
    )
    .await;

    assert_eq!(details["events"][0]["event_id"], "evt-60");
    assert_eq!(filtered["events"][0]["event_id"], "evt-60");
    assert_eq!(filtered["events"][0]["truncated"], true);
    resolve.assert_calls(2);
    list_events.assert_calls(2);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_events_requires_action_specific_inputs_before_auth() {
    let context = test_context!();
    let client = spawn_mcp_client(&context, &["--server", "http://127.0.0.1:9"]).await;

    let details_error = call_tool_error_text(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "run_123",
            "action": "details"
        }),
    )
    .await;
    let search_error = call_tool_error_text(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "run_123",
            "action": "search"
        }),
    )
    .await;

    assert!(details_error.contains("event_ids"), "{details_error}");
    assert!(
        !details_error.contains("fabro auth login"),
        "{details_error}"
    );
    assert!(search_error.contains("query"), "{search_error}");
    assert!(!search_error.contains("fabro auth login"), "{search_error}");
    assert_eq!(client.list_tools().await.unwrap().len(), 6);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_events_desc_after_offset_and_limit_page_over_requested_order() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let run_id = unique_run_id();
    let resolve = mock_resolved_run(&server, "nightly", &run_id);
    let events = (1..=5)
        .map(|sequence| {
            serde_json::json!({
                "seq": sequence,
                "id": format!("evt-{sequence}"),
                "ts": format!("2026-04-05T12:00:0{sequence}Z"),
                "run_id": run_id,
                "event": "run.started",
                "properties": { "name": format!("event {sequence}") },
                "actor": null
            })
        })
        .collect::<Vec<_>>();
    let first_event = events[0].clone();
    let limited_events = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param("limit", "1");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": [first_event],
                "meta": { "has_more": true }
            }));
    });
    let full_events = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param_missing("limit")
            .query_param_missing("since_seq");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": events,
                "meta": { "has_more": false }
            }));
    });
    let after_events = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param("since_seq", "2")
            .query_param("limit", "3");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": [
                    {
                        "seq": 2,
                        "id": "evt-2",
                        "ts": "2026-04-05T12:00:02Z",
                        "run_id": run_id,
                        "event": "run.started",
                        "properties": { "name": "event 2" },
                        "actor": null
                    },
                    {
                        "seq": 3,
                        "id": "evt-3",
                        "ts": "2026-04-05T12:00:03Z",
                        "run_id": run_id,
                        "event": "run.started",
                        "properties": { "name": "event 3" },
                        "actor": null
                    },
                    {
                        "seq": 4,
                        "id": "evt-4",
                        "ts": "2026-04-05T12:00:04Z",
                        "run_id": run_id,
                        "event": "run.started",
                        "properties": { "name": "event 4" },
                        "actor": null
                    }
                ],
                "meta": { "has_more": true }
            }));
    });
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;

    let desc = call_tool_json(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "nightly",
            "action": "list",
            "direction": "desc",
            "first": 1
        }),
    )
    .await;
    let paged = call_tool_json(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "nightly",
            "action": "list",
            "after": 2,
            "offset": 1,
            "limit": 2
        }),
    )
    .await;

    assert_eq!(desc["events"][0]["event_id"], "evt-5");
    assert_eq!(desc["next_cursor"], 5);
    assert_eq!(paged["events"][0]["event_id"], "evt-3");
    assert_eq!(paged["events"][1]["event_id"], "evt-4");
    assert_eq!(paged["next_cursor"], 5);
    resolve.assert_calls(2);
    limited_events.assert_calls(0);
    full_events.assert();
    after_events.assert();
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_events_desc_cursor_continues_to_older_events() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let run_id = unique_run_id();
    let resolve = mock_resolved_run(&server, "nightly", &run_id);
    let events = (1..=5)
        .map(|sequence| {
            serde_json::json!({
                "seq": sequence,
                "id": format!("evt-{sequence}"),
                "ts": format!("2026-04-05T12:00:0{sequence}Z"),
                "run_id": run_id,
                "event": "run.started",
                "properties": { "name": format!("event {sequence}") },
                "actor": null
            })
        })
        .collect::<Vec<_>>();
    let full_events = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param_missing("limit")
            .query_param_missing("since_seq");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": events,
                "meta": { "has_more": false }
            }));
    });
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;

    let first_page = call_tool_json(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "nightly",
            "action": "list",
            "direction": "desc",
            "first": 2
        }),
    )
    .await;
    let second_page = call_tool_json(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "nightly",
            "action": "list",
            "direction": "desc",
            "after": first_page["next_cursor"],
            "first": 2
        }),
    )
    .await;

    assert_eq!(first_page["events"][0]["event_id"], "evt-5");
    assert_eq!(first_page["events"][1]["event_id"], "evt-4");
    assert_eq!(first_page["next_cursor"], 4);
    assert_eq!(second_page["events"][0]["event_id"], "evt-3");
    assert_eq!(second_page["events"][1]["event_id"], "evt-2");
    assert_eq!(second_page["next_cursor"], 2);
    resolve.assert_calls(2);
    full_events.assert_calls(2);
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_events_offset_beyond_fetch_cap_reaches_later_pages() {
    let context = test_context!();
    let server = MockServer::start();
    let target_url = format!("{}/api/v1", server.base_url());
    let target: fabro_client::ServerTarget = target_url.parse().unwrap();
    seed_dev_token_auth(&context.home_dir, &target, TEST_DEV_TOKEN);
    let run_id = unique_run_id();
    let resolve = mock_resolved_run(&server, "nightly", &run_id);
    let events = (1..=300)
        .map(|sequence| {
            serde_json::json!({
                "seq": sequence,
                "id": format!("evt-{sequence}"),
                "ts": "2026-04-05T12:00:00Z",
                "run_id": run_id,
                "event": "run.started",
                "properties": { "name": format!("event {sequence}") },
                "actor": null
            })
        })
        .collect::<Vec<_>>();
    let first_251_events = events.iter().take(251).cloned().collect::<Vec<_>>();
    let bounded_events = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param("limit", "251");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": first_251_events,
                "meta": { "has_more": true }
            }));
    });
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;

    let paged = call_tool_json(
        &client,
        "fabro_run_events",
        serde_json::json!({
            "run_id": "nightly",
            "action": "list",
            "offset": 250,
            "first": 1
        }),
    )
    .await;

    assert_eq!(paged["events"][0]["event_id"], "evt-251");
    assert_eq!(paged["next_cursor"], 252);
    resolve.assert();
    bounded_events.assert();
    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_tool_auth_error_mentions_login() {
    let context = test_context!();
    let harness =
        RealAuthHarness::start_with_dev_token(fabro_test::GitHubAppState::default()).await;
    let target_url = harness.api_target();
    let client = spawn_mcp_client(&context, &["--server", &target_url]).await;

    let error = call_tool_error_text(
        &client,
        "fabro_run_search",
        serde_json::json!({ "first": 1 }),
    )
    .await;

    assert!(
        error.contains("Run `fabro auth login` to authenticate."),
        "{error}"
    );
    assert_eq!(client.list_tools().await.unwrap().len(), 6);

    client
        .shutdown()
        .await
        .expect("MCP client should shut down");
    harness.shutdown().await;
}

fn expected_claude_desktop_config_path(home_dir: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir
            .join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude_desktop_config.json")
    }
    #[cfg(target_os = "linux")]
    {
        home_dir
            .join(".config")
            .join("Claude")
            .join("claude_desktop_config.json")
    }
    #[cfg(target_os = "windows")]
    {
        home_dir
            .join("AppData")
            .join("Roaming")
            .join("Claude")
            .join("claude_desktop_config.json")
    }
}

struct McpStdioFixture {
    command:     Vec<String>,
    env:         HashMap<String, String>,
    current_dir: PathBuf,
}

fn mcp_stdio_fixture(context: &fabro_test::TestContext, extra_args: &[&str]) -> McpStdioFixture {
    let mut command = vec![
        env!("CARGO_BIN_EXE_fabro").to_string(),
        "mcp".to_string(),
        "start".to_string(),
    ];
    command.extend(extra_args.iter().map(|arg| (*arg).to_string()));

    let mut env = fabro_test::isolated_env(&context.home_dir);
    env.insert(
        "FABRO_HOME".to_string(),
        context.home_dir.join(".fabro").display().to_string(),
    );

    McpStdioFixture {
        command,
        env,
        current_dir: context.temp_dir.clone(),
    }
}

fn write_mcp_server_settings(
    context: &mut fabro_test::TestContext,
    storage_dir: &Path,
    socket_path: Option<&Path>,
) {
    context.manage_storage_dir(storage_dir);
    let cli_target = socket_path.map_or_else(String::new, |path| {
        format!(
            r#"
[cli.target]
type = "unix"
path = "{}"
"#,
            path.display()
        )
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"_version = 1

[server.storage]
root = "{}"

[server.auth]
methods = ["dev-token"]
{cli_target}"#,
            storage_dir.display()
        ),
    );
}

async fn spawn_mcp_client(context: &fabro_test::TestContext, extra_args: &[&str]) -> McpClient {
    let fixture = mcp_stdio_fixture(context, extra_args);
    spawn_mcp_client_from_fixture(fixture).await
}

async fn spawn_mcp_client_from_fixture(fixture: McpStdioFixture) -> McpClient {
    let config = McpServerSettings {
        name:                 "fabro-under-test".to_string(),
        transport:            McpTransport::Stdio {
            command: fixture.command,
            env:     fixture.env,
        },
        current_dir:          Some(fixture.current_dir),
        clear_env:            true,
        startup_timeout_secs: 10,
        tool_timeout_secs:    30,
    };
    let client = McpClient::new(&config).expect("MCP client should build");
    client
        .initialize(config.startup_timeout())
        .await
        .expect("MCP server should initialize");
    client
}

async fn call_tool_json(
    client: &McpClient,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let result = client
        .call_tool(name, arguments, std::time::Duration::from_secs(30))
        .await
        .expect("tool call should complete");
    assert_ne!(
        result.is_error,
        Some(true),
        "tool returned error: {result:?}"
    );
    let text = result
        .content
        .first()
        .and_then(|content| serde_json::to_value(content).ok())
        .and_then(|content| content["text"].as_str().map(ToOwned::to_owned))
        .expect("tool result should include text fallback");
    assert!(!text.starts_with('{') && !text.starts_with('['));
    result
        .structured_content
        .expect("tool result should include structured content")
}

async fn call_tool_error_text(
    client: &McpClient,
    name: &str,
    arguments: serde_json::Value,
) -> String {
    let result = client
        .call_tool(name, arguments, std::time::Duration::from_secs(30))
        .await
        .expect("tool call should complete");
    assert_eq!(result.is_error, Some(true), "tool should return error");
    result
        .content
        .first()
        .and_then(|content| serde_json::to_value(content).ok())
        .and_then(|content| content["text"].as_str().map(ToOwned::to_owned))
        .expect("tool error should include text")
}

async fn create_mcp_run(client: &McpClient, workflow: PathBuf, start: bool) -> String {
    let create = call_tool_json(
        client,
        "fabro_run_create",
        serde_json::json!({
            "runs": [{
                "workflow": workflow,
                "dry_run": true,
                "auto_approve": true,
                "labels": { "source": "mcp-test" },
                "start": start
            }]
        }),
    )
    .await;
    create["runs"][0]["run_id"]
        .as_str()
        .expect("create result should include run id")
        .to_string()
}

fn seed_oauth_auth(
    home_dir: &Path,
    target: &fabro_client::ServerTarget,
    access_token: &str,
    refresh_token: &str,
) {
    let now = Utc::now();
    AuthStore::new(home_dir.join(".fabro/auth.json"))
        .put(
            target,
            AuthEntry::OAuth(OAuthEntry {
                access_token:             access_token.to_string(),
                access_token_expires_at:  now - ChronoDuration::minutes(1),
                refresh_token:            refresh_token.to_string(),
                refresh_token_expires_at: now + ChronoDuration::days(30),
                subject:                  StoredSubject {
                    idp_issuer:  "https://github.com".to_string(),
                    idp_subject: "12345".to_string(),
                    login:       "octocat".to_string(),
                    name:        "The Octocat".to_string(),
                    email:       "octocat@example.com".to_string(),
                },
                logged_in_at:             now,
            }),
        )
        .unwrap_or_else(|err| panic!("failed to seed OAuth auth: {err}"));
}

fn normalize_run_search(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(runs) = value["runs"].as_array_mut() {
        for run in runs {
            run["run_id"] = serde_json::json!("[RUN_ID]");
            run["created_at"] = serde_json::json!("[TIMESTAMP]");
            if run["started_at"].is_string() {
                run["started_at"] = serde_json::json!("[TIMESTAMP]");
            }
            if run["completed_at"].is_string() {
                run["completed_at"] = serde_json::json!("[TIMESTAMP]");
            }
            if run["source_directory"].is_string() {
                run["source_directory"] = serde_json::json!("[SOURCE_DIRECTORY]");
            }
            if run["repo_origin_url"].is_string() {
                run["repo_origin_url"] = serde_json::json!("[REPO_ORIGIN_URL]");
            }
        }
    }
    value
}

fn normalize_gather(mut value: serde_json::Value) -> serde_json::Value {
    value["elapsed_seconds"] = serde_json::json!("[ELAPSED]");
    if let Some(runs) = value["runs"].as_array_mut() {
        for run in runs {
            run["run_id"] = serde_json::json!("[RUN_ID]");
            run["created_at"] = serde_json::json!("[TIMESTAMP]");
            if run["started_at"].is_string() {
                run["started_at"] = serde_json::json!("[TIMESTAMP]");
            }
            if run["completed_at"].is_string() {
                run["completed_at"] = serde_json::json!("[TIMESTAMP]");
            }
            if run["source_directory"].is_string() {
                run["source_directory"] = serde_json::json!("[SOURCE_DIRECTORY]");
            }
            if run["repo_origin_url"].is_string() {
                run["repo_origin_url"] = serde_json::json!("[REPO_ORIGIN_URL]");
            }
        }
    }
    value
}

fn run_id_with_timestamp(timestamp: &str, sequence: u128) -> String {
    let timestamp = DateTime::parse_from_rfc3339(timestamp)
        .expect("test timestamp should parse")
        .with_timezone(&Utc);
    RunId::with_timestamp(timestamp, sequence).to_string()
}

fn mock_resolved_run_json<'a>(
    server: &'a MockServer,
    selector: &str,
    body: serde_json::Value,
    authorization: Option<&str>,
) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        let when = when
            .method(GET)
            .path("/api/v1/runs/resolve")
            .query_param("selector", selector);
        if let Some(authorization) = authorization {
            when.header("authorization", authorization);
        }
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(body);
    })
}
