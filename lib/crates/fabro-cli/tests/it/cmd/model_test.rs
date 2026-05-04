use assert_cmd::Command;
use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

fn remove_provider_env(cmd: &mut Command) -> &mut Command {
    cmd.env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("KIMI_API_KEY")
        .env_remove("ZAI_API_KEY")
        .env_remove("MINIMAX_API_KEY")
        .env_remove("INCEPTION_API_KEY")
}

fn model_json(id: &str, provider: &str, configured: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "display_name": id,
        "provider": provider,
        "family": "test",
        "aliases": [],
        "limits": {
            "context_window": 131_072,
            "max_output": 4096
        },
        "training": null,
        "knowledge_cutoff": null,
        "features": {
            "tools": true,
            "vision": false,
            "reasoning": false,
            "effort": false
        },
        "costs": {
            "input_cost_per_mtok": 1.0,
            "output_cost_per_mtok": 2.0,
            "cache_input_cost_per_mtok": null
        },
        "estimated_output_tps": 42.0,
        "default": false,
        "configured": configured
    })
}

fn mock_model_list(
    server: &MockServer,
    models: impl IntoIterator<Item = serde_json::Value>,
) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method("GET").path("/api/v1/models");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": models.into_iter().collect::<Vec<_>>(),
                "meta": { "has_more": false }
            }));
    })
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Test model availability by sending a simple prompt

    Usage: fabro model test [OPTIONS]

    Options:
          --json                 Output as JSON [env: FABRO_JSON=]
          --server <SERVER>      Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug                Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -p, --provider <PROVIDER>  Filter by provider
      -m, --model <MODEL>        Test a specific model
          --no-upgrade-check     Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --deep                 Run a multi-turn tool-use test (catches reasoning round-trip bugs)
          --quiet                Suppress non-essential output [env: FABRO_QUIET=]
          --verbose              Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                 Print help
    ----- stderr -----
    ");
}

#[test]
fn model_test_unknown_model_errors() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test", "--model", "nonexistent-model-xyz"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Testing nonexistent-model-xyz... done
      × Unknown model: nonexistent-model-xyz
    ");
}

#[test]
fn single_model_skip_exits_nonzero() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test", "--model", "gemini-3.1-pro-preview"]);
    remove_provider_env(&mut cmd);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    MODEL                   PROVIDER  ALIASES     CONTEXT          COST     SPEED  RESULT         
     gemini-3.1-pro-preview  gemini    gemini-pro       1m  $2.0 / $12.0  85 tok/s  not configured
    ----- stderr -----
    Testing gemini-3.1-pro-preview... done
      × 1 model(s) failed
    ");
}

#[test]
fn bulk_skip_exits_zero_and_prints_summary() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test"]);
    remove_provider_env(&mut cmd);

    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "bulk skip should exit 0:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Skipped"),
        "should report skipped models:\n{stderr}"
    );
}

#[test]
fn json_output_includes_skipped_models() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args([
        "model",
        "test",
        "--model",
        "gemini-3.1-pro-preview",
        "--json",
    ]);
    remove_provider_env(&mut cmd);

    let output = cmd.output().expect("failed to execute model test");
    assert!(
        !output.status.success(),
        "expected single-model skip to exit non-zero:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON output");

    assert_eq!(json["failures"], 1);
    assert_eq!(json["skipped"], 1);
    assert_eq!(json["results"][0]["result"], "skip");
}

#[test]
fn model_test_does_not_announce_unconfigured() {
    let context = test_context!();
    let server = MockServer::start();
    context.set_http_target(&server.base_url());
    let list = mock_model_list(&server, [
        model_json("claude-opus-4-7", "anthropic", true),
        model_json("gpt-5.2", "openai", false),
    ]);
    let configured_test = server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/models/claude-opus-4-7/test");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "model_id": "claude-opus-4-7",
                "status": "ok"
            }));
    });
    let unconfigured_test = server.mock(|when, then| {
        when.method("POST").path("/api/v1/models/gpt-5.2/test");
        then.status(500)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "errors": [{
                    "status": "500",
                    "title": "should not be called"
                }]
            }));
    });

    let mut cmd = context.command();
    cmd.args(["model", "test"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        output.status.success(),
        "model test should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Testing claude-opus-4-7..."));
    assert!(!stderr.contains("Testing gpt-5.2..."));
    list.assert();
    configured_test.assert();
    unconfigured_test.assert_calls(0);
}

#[test]
fn model_test_skipped_footer_sources_from_listing() {
    let context = test_context!();
    let server = MockServer::start();
    context.set_http_target(&server.base_url());
    mock_model_list(&server, [
        model_json("claude-opus-4-7", "anthropic", true),
        model_json("gpt-5.2", "openai", false),
    ]);
    server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/models/claude-opus-4-7/test");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "model_id": "claude-opus-4-7",
                "status": "ok"
            }));
    });

    let mut cmd = context.command();
    cmd.args(["model", "test"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        output.status.success(),
        "model test should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Skipped 1 model(s) (no credentials: openai)"));
}

#[test]
fn model_test_post_list_race_is_a_failure() {
    let context = test_context!();
    let server = MockServer::start();
    context.set_http_target(&server.base_url());
    mock_model_list(&server, [model_json("claude-opus-4-7", "anthropic", true)]);
    server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/models/claude-opus-4-7/test");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "model_id": "claude-opus-4-7",
                "status": "skip"
            }));
    });

    let mut cmd = context.command();
    cmd.args(["model", "test"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        !output.status.success(),
        "post-list skip should fail:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("provider became unconfigured after listing"));
    assert!(stderr.contains("1 model(s) failed"));
    assert!(!stderr.contains("Skipped"));
}

#[test]
fn model_test_json_partitions_skip_and_fail() {
    let context = test_context!();
    let server = MockServer::start();
    context.set_http_target(&server.base_url());
    mock_model_list(&server, [
        model_json("gpt-5.2", "openai", false),
        model_json("claude-opus-4-7", "anthropic", true),
    ]);
    let unconfigured_test = server.mock(|when, then| {
        when.method("POST").path("/api/v1/models/gpt-5.2/test");
        then.status(500);
    });
    server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/models/claude-opus-4-7/test");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "model_id": "claude-opus-4-7",
                "status": "skip"
            }));
    });

    let mut cmd = context.command();
    cmd.args(["model", "test", "--json"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        !output.status.success(),
        "race failure should make json command fail:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    unconfigured_test.assert_calls(0);
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("invalid JSON output");
    assert_eq!(json["total"], 2);
    assert_eq!(json["skipped"], 1);
    assert_eq!(json["failures"], 1);
    assert_eq!(json["results"][0]["model"], "gpt-5.2");
    assert_eq!(json["results"][0]["result"], "skip");
    assert_eq!(json["results"][0]["detail"], "not configured");
    assert_eq!(json["results"][1]["model"], "claude-opus-4-7");
    assert_eq!(json["results"][1]["result"], "fail");
    assert_eq!(
        json["results"][1]["error"],
        "provider became unconfigured after listing"
    );
}
