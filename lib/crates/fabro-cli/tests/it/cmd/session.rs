use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["session", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Run a persistent Fabro agent session

    Usage: fabro session [OPTIONS] --prompt <PROMPT>

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
      -p, --prompt <PROMPT>            Task prompt
          --provider <PROVIDER>        LLM provider (anthropic, openai, gemini, kimi, zai, minimax, inception)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --model <MODEL>              Model name (defaults per provider)
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --permissions <LEVEL>        Permission level for tool execution [possible values: read-only, read-write, full]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn prompt_creates_session_and_streams_turn_events() {
    let context = test_context!();
    let server = MockServer::start();
    let session_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn_id = "01BX5ZZKBKACTAV9WEVGEMMVRZ";

    let create = server.mock(|when, then| {
        when.method("POST").path("/api/v1/sessions");
        then.status(201)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "id": session_id,
                "title": "say hello",
                "status": "idle",
                "working_dir": context.temp_dir.to_string_lossy(),
                "provider": "openai",
                "model": "gpt-5.4-mini",
                "permissions": "read-write",
                "created_at": "2026-04-05T12:00:00Z",
                "updated_at": "2026-04-05T12:00:00Z",
                "deleted_at": null,
                "runtime_context": []
            }));
    });
    let submit = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/sessions/{session_id}/turns"));
        then.status(200)
            .header("Content-Type", "text/event-stream")
            .body(format!(
                "data: {}\n\ndata: {}\n\n",
                serde_json::json!({
                    "seq": 1,
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "event": "turn.assistant_message",
                    "properties": {"text": "Hello from server"},
                    "ts": "2026-04-05T12:00:01Z"
                }),
                serde_json::json!({
                    "seq": 2,
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "event": "turn.succeeded",
                    "properties": {"turn_id": turn_id},
                    "ts": "2026-04-05T12:00:02Z"
                })
            ));
    });

    let output = context
        .command()
        .args([
            "session",
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--provider",
            "openai",
            "--model",
            "gpt-5.4-mini",
            "-p",
            "say hello",
        ])
        .output()
        .expect("session command should execute");

    create.assert();
    submit.assert();
    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Hello from server\n"
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}
