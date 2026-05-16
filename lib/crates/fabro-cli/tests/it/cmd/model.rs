#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

fn successful_stdout(mut cmd: assert_cmd::Command) -> String {
    let assert = cmd.assert().success().stderr("");
    String::from_utf8(assert.get_output().stdout.clone())
        .expect("model list stdout should be UTF-8")
}

fn assert_model_list_table(stdout: &str) {
    let mut lines = stdout.lines();
    let header = lines.next().expect("model list should render a header");
    assert!(header.contains("MODEL"), "missing MODEL column: {header}");
    assert!(
        header.contains("PROVIDER"),
        "missing PROVIDER column: {header}"
    );
    assert!(
        header.contains("CONTEXT"),
        "missing CONTEXT column: {header}"
    );
    assert!(
        lines.any(|line| !line.trim().is_empty()),
        "model list should render at least one model row"
    );
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List and test LLM models

    Usage: fabro model [OPTIONS] [COMMAND]

    Commands:
      list  List available models
      test  Test model availability by sending a simple prompt
      help  Print this message or the help of the given subcommand(s)

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
fn bare() {
    let context = test_context!();
    let stdout = successful_stdout(context.model());
    assert_model_list_table(&stdout);
}

#[test]
fn list() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.arg("list");
    let stdout = successful_stdout(cmd);
    assert_model_list_table(&stdout);
}

#[test]
fn list_provider() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--provider", "anthropic"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    MODEL              PROVIDER   ALIASES                CONTEXT          COST      SPEED 
     claude-haiku-4-5   anthropic  haiku, claude-haiku       200k   $0.8 / $4.0  100 tok/s 
     claude-opus-4-6    anthropic                              1m  $5.0 / $25.0   25 tok/s 
     claude-opus-4-7    anthropic  opus, claude-opus           1m  $5.0 / $25.0   25 tok/s 
     claude-sonnet-4-5  anthropic                            200k  $3.0 / $15.0   50 tok/s 
     claude-sonnet-4-6  anthropic  sonnet, claude-sonnet     200k  $3.0 / $15.0   50 tok/s
    ----- stderr -----
    ");
}

#[test]
fn list_query() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--query", "opus"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    MODEL            PROVIDER   ALIASES            CONTEXT          COST     SPEED 
     claude-opus-4-6  anthropic                          1m  $5.0 / $25.0  25 tok/s 
     claude-opus-4-7  anthropic  opus, claude-opus       1m  $5.0 / $25.0  25 tok/s
    ----- stderr -----
    ");
}

#[test]
fn list_query_aliases() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--query", "codex"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    MODEL                PROVIDER  ALIASES      CONTEXT          COST       SPEED 
     gpt-5.2-codex        openai                      1m  $1.8 / $14.0   100 tok/s 
     gpt-5.3-codex        openai    codex             1m  $1.8 / $14.0   100 tok/s 
     gpt-5.3-codex-spark  openai    codex-spark     131k         - / -  1000 tok/s
    ----- stderr -----
    ");
}

#[test]
fn list_query_case_insensitive() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--query", "OPUS"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    MODEL            PROVIDER   ALIASES            CONTEXT          COST     SPEED 
     claude-opus-4-6  anthropic                          1m  $5.0 / $25.0  25 tok/s 
     claude-opus-4-7  anthropic  opus, claude-opus       1m  $5.0 / $25.0  25 tok/s
    ----- stderr -----
    ");
}

#[test]
fn list_unknown_provider_returns_empty() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--provider", "not-a-provider"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    MODEL  PROVIDER  ALIASES  CONTEXT  COST  SPEED
    ----- stderr -----
    ");
}

#[test]
fn list_uses_configured_server_target_without_server_flag() {
    let context = test_context!();
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("GET");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [{
                        "id": "remote-model",
                        "display_name": "Remote Model",
                        "provider": "openai",
                        "family": "test",
                        "aliases": ["remote"],
                        "limits": {
                            "context_window": 131_072,
                            "max_output": 4096
                        },
                        "training": null,
                        "knowledge_cutoff": null,
                        "features": {
                            "tools": true,
                            "vision": false,
                            "reasoning": false
                        },
                        "costs": {
                            "input_cost_per_mtok": 1.0,
                            "output_cost_per_mtok": 2.0,
                            "cache_input_cost_per_mtok": null
                        },
                        "estimated_output_tps": 42.0,
                        "default": false,
                        "configured": false
                    }],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    context.set_http_target(&server.base_url());

    let mut cmd = context.model();
    cmd.args(["list", "--json"]);
    let output = cmd.assert().success().get_output().stdout.clone();
    let models: serde_json::Value =
        serde_json::from_slice(&output).expect("model list json should parse");

    mock.assert();
    assert_eq!(models.as_array().map(Vec::len), Some(1));
    assert_eq!(models[0]["id"].as_str(), Some("remote-model"));
}

#[test]
fn list_uses_fabro_config_for_machine_settings() {
    let context = test_context!();
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("GET");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [{
                        "id": "remote-model",
                        "display_name": "Remote Model",
                        "provider": "openai",
                        "family": "test",
                        "aliases": ["remote"],
                        "limits": {
                            "context_window": 131_072,
                            "max_output": 4096
                        },
                        "training": null,
                        "knowledge_cutoff": null,
                        "features": {
                            "tools": true,
                            "vision": false,
                            "reasoning": false
                        },
                        "costs": {
                            "input_cost_per_mtok": 1.0,
                            "output_cost_per_mtok": 2.0,
                            "cache_input_cost_per_mtok": null
                        },
                        "estimated_output_tps": 42.0,
                        "default": false,
                        "configured": false
                    }],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("custom-settings.toml");
    std::fs::write(
        &config_path,
        format!(
            "_version = 1\n\n[cli.target]\ntype = \"http\"\nurl = \"{}/api/v1\"\n",
            server.base_url()
        ),
    )
    .unwrap();

    let mut cmd = context.model();
    cmd.args(["list", "--json"]);
    cmd.env("FABRO_CONFIG", &config_path);
    let output = cmd.assert().success().get_output().stdout.clone();
    let models: serde_json::Value =
        serde_json::from_slice(&output).expect("model list json should parse");

    mock.assert();
    assert_eq!(models.as_array().map(Vec::len), Some(1));
    assert_eq!(models[0]["id"].as_str(), Some("remote-model"));
}
