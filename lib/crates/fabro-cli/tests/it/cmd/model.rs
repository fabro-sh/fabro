use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

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
    fabro_snapshot!(context.filters(), context.model(), @"
    success: true
    exit_code: 0
    ----- stdout -----
    MODEL                               PROVIDER   ALIASES                  CONTEXT            COST       SPEED 
     claude-opus-4-6                     anthropic  opus, claude-opus             1m    $5.0 / $25.0    25 tok/s 
     claude-sonnet-4-5                   anthropic                              200k    $3.0 / $15.0    50 tok/s 
     claude-sonnet-4-6                   anthropic  sonnet, claude-sonnet       200k    $3.0 / $15.0    50 tok/s 
     claude-haiku-4-5                    anthropic  haiku, claude-haiku         200k     $0.8 / $4.0   100 tok/s 
     gpt-5.2                             openai     gpt5                          1m    $1.8 / $14.0    65 tok/s 
     gpt-5-mini                          openai     gpt5-mini                     1m     $0.2 / $2.0    70 tok/s 
     gpt-5.2-codex                       openai                                   1m    $1.8 / $14.0   100 tok/s 
     gpt-5.3-codex                       openai     codex                         1m    $1.8 / $14.0   100 tok/s 
     gpt-5.3-codex-spark                 openai     codex-spark                 131k           - / -  1000 tok/s 
     gpt-5.4                             openai     gpt54, gpt-54                 1m    $2.5 / $15.0    70 tok/s 
     gpt-5.4-pro                         openai     gpt54-pro, gpt-54-pro         1m  $30.0 / $180.0    20 tok/s 
     gpt-5.4-mini                        openai     gpt54-mini, gpt-54-mini     400k     $0.8 / $4.5   140 tok/s 
     gemini-3.1-pro-preview              gemini     gemini-pro                    1m    $2.0 / $12.0    85 tok/s 
     gemini-3.1-pro-preview-customtools  gemini     gemini-customtools            1m    $2.0 / $12.0    85 tok/s 
     gemini-3-flash-preview              gemini     gemini-flash                  1m     $0.5 / $3.0   150 tok/s 
     gemini-3.1-flash-lite-preview       gemini     gemini-flash-lite             1m     $0.2 / $1.5   200 tok/s 
     kimi-k2.5                           kimi       kimi                        262k     $0.6 / $3.0    50 tok/s 
     glm-4.7                             zai        glm, glm4                   203k     $0.6 / $2.2   100 tok/s 
     minimax-m2.5                        minimax    minimax                     197k     $0.3 / $1.2    45 tok/s 
     mercury-2                           inception  mercury                     131k     $0.2 / $0.8  1000 tok/s
    ----- stderr -----
    ");
}

#[test]
fn list() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.arg("list");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    MODEL                               PROVIDER   ALIASES                  CONTEXT            COST       SPEED 
     claude-opus-4-6                     anthropic  opus, claude-opus             1m    $5.0 / $25.0    25 tok/s 
     claude-sonnet-4-5                   anthropic                              200k    $3.0 / $15.0    50 tok/s 
     claude-sonnet-4-6                   anthropic  sonnet, claude-sonnet       200k    $3.0 / $15.0    50 tok/s 
     claude-haiku-4-5                    anthropic  haiku, claude-haiku         200k     $0.8 / $4.0   100 tok/s 
     gpt-5.2                             openai     gpt5                          1m    $1.8 / $14.0    65 tok/s 
     gpt-5-mini                          openai     gpt5-mini                     1m     $0.2 / $2.0    70 tok/s 
     gpt-5.2-codex                       openai                                   1m    $1.8 / $14.0   100 tok/s 
     gpt-5.3-codex                       openai     codex                         1m    $1.8 / $14.0   100 tok/s 
     gpt-5.3-codex-spark                 openai     codex-spark                 131k           - / -  1000 tok/s 
     gpt-5.4                             openai     gpt54, gpt-54                 1m    $2.5 / $15.0    70 tok/s 
     gpt-5.4-pro                         openai     gpt54-pro, gpt-54-pro         1m  $30.0 / $180.0    20 tok/s 
     gpt-5.4-mini                        openai     gpt54-mini, gpt-54-mini     400k     $0.8 / $4.5   140 tok/s 
     gemini-3.1-pro-preview              gemini     gemini-pro                    1m    $2.0 / $12.0    85 tok/s 
     gemini-3.1-pro-preview-customtools  gemini     gemini-customtools            1m    $2.0 / $12.0    85 tok/s 
     gemini-3-flash-preview              gemini     gemini-flash                  1m     $0.5 / $3.0   150 tok/s 
     gemini-3.1-flash-lite-preview       gemini     gemini-flash-lite             1m     $0.2 / $1.5   200 tok/s 
     kimi-k2.5                           kimi       kimi                        262k     $0.6 / $3.0    50 tok/s 
     glm-4.7                             zai        glm, glm4                   203k     $0.6 / $2.2   100 tok/s 
     minimax-m2.5                        minimax    minimax                     197k     $0.3 / $1.2    45 tok/s 
     mercury-2                           inception  mercury                     131k     $0.2 / $0.8  1000 tok/s
    ----- stderr -----
    ");
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
     claude-opus-4-6    anthropic  opus, claude-opus           1m  $5.0 / $25.0   25 tok/s 
     claude-sonnet-4-5  anthropic                            200k  $3.0 / $15.0   50 tok/s 
     claude-sonnet-4-6  anthropic  sonnet, claude-sonnet     200k  $3.0 / $15.0   50 tok/s 
     claude-haiku-4-5   anthropic  haiku, claude-haiku       200k   $0.8 / $4.0  100 tok/s
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
     claude-opus-4-6  anthropic  opus, claude-opus       1m  $5.0 / $25.0  25 tok/s
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
     claude-opus-4-6  anthropic  opus, claude-opus       1m  $5.0 / $25.0  25 tok/s
    ----- stderr -----
    ");
}

#[test]
fn list_invalid_provider_errors() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--provider", "not-a-provider"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: unknown provider: not-a-provider
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
                            "reasoning": false,
                            "effort": false
                        },
                        "costs": {
                            "input_cost_per_mtok": 1.0,
                            "output_cost_per_mtok": 2.0,
                            "cache_input_cost_per_mtok": null
                        },
                        "estimated_output_tps": 42.0,
                        "default": false
                    }],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "_version = 1\n\n[cli.target]\ntype = \"http\"\nurl = \"{}/api/v1\"\n",
            server.base_url()
        ),
    );

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
                            "reasoning": false,
                            "effort": false
                        },
                        "costs": {
                            "input_cost_per_mtok": 1.0,
                            "output_cost_per_mtok": 2.0,
                            "cache_input_cost_per_mtok": null
                        },
                        "estimated_output_tps": 42.0,
                        "default": false
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
