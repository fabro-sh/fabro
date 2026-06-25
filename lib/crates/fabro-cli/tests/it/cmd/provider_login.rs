use fabro_test::{fabro_snapshot, test_context};
use httpmock::prelude::*;
use serde_json::json;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["provider", "login", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Log in to an LLM provider

    Usage: fabro provider login [OPTIONS] --provider <PROVIDER>

    Options:
          --json                 Output as JSON [env: FABRO_JSON=]
          --server <SERVER>      Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug                Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --provider <PROVIDER>  LLM provider to authenticate with
          --api-key-stdin        Read an API key from stdin instead of prompting
          --no-upgrade-check     Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                Suppress non-essential output [env: FABRO_QUIET=]
          --verbose              Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                 Print help
    ----- stderr -----
    ");
}

#[test]
fn provider_login_rejects_json() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "provider", "login", "--provider", "anthropic"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json is not supported for this command"));
}

#[test]
fn api_key_login_uses_server_provider_catalog_for_remote_target() {
    let context = test_context!();
    let server = MockServer::start();
    let providers = server.mock(|when, then| {
        when.method(GET).path("/api/v1/providers");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "data": [{
                    "id": "openrouter",
                    "display_name": "OpenRouter",
                    "adapter": "openai_compatible",
                    "base_url": "https://openrouter.ai/api/v1",
                    "api_key_url": "https://openrouter.ai/keys",
                    "priority": 25,
                    "aliases": [],
                    "model_count": 1,
                    "default_model": "openrouter/test",
                    "configured": false,
                    "expected_secret_name": "OPENROUTER_API_KEY"
                }]
            }));
    });
    let validation = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/providers/openrouter/credentials/test")
            .json_body(json!({ "api_key": "sk-or-v1-test" }));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(json!({ "ok": true }));
    });
    let secret = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/secrets")
            .json_body_includes(
                r#"{
                "name": "OPENROUTER_API_KEY",
                "value": "sk-or-v1-test",
                "type": "token"
            }"#,
            );
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "name": "OPENROUTER_API_KEY",
                "type": "token",
                "created_at": "2026-06-25T00:00:00Z",
                "updated_at": "2026-06-25T00:00:00Z"
            }));
    });

    context
        .command()
        .args([
            "provider",
            "login",
            "--server",
            &server.url(""),
            "--provider",
            "openrouter",
            "--api-key-stdin",
            "--no-upgrade-check",
        ])
        .write_stdin("sk-or-v1-test\n")
        .assert()
        .success();

    providers.assert();
    validation.assert();
    secret.assert();
}
