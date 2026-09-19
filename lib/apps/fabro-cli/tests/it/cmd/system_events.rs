use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["system", "events", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Stream run events from the server

    Usage: fabro system events [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --run-id <RUN_IDS>           Filter by run ID (repeatable)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn system_events_renders_text_lines_from_sse_payloads() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = crate::support::unique_run_id();
    // One `RunStreamItem`: the platform record that ended the run.
    let payload = serde_json::json!({
        "run_id": run_id,
        "stream_seq": 7,
        "kind": "platform",
        "id": "7",
        "recorded_at": 1_775_390_400_000_u64,
        "item": {
            "seq": 7,
            "recorded_at": 1_775_390_400_000_u64,
            "record": {
                "kind": "run.lifecycle",
                "transition": "succeeded",
                "status": { "kind": "succeeded", "reason": "completed" }
            }
        }
    });
    let attach_mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/attach")
            .query_param("run_id", run_id.as_str());
        then.status(200)
            .header("Content-Type", "text/event-stream")
            .body(format!("data: {payload}\n\n"));
    });

    let output = context
        .command()
        .args([
            "system",
            "events",
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--run-id",
            &run_id,
        ])
        .output()
        .expect("command should run");

    assert!(output.status.success(), "system events failed");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        stdout.trim(),
        format!("2026-04-05T12:00:00+00:00 {} run.lifecycle", &run_id[..12])
    );
    attach_mock.assert();
}

#[test]
fn system_events_json_emits_raw_sse_payloads() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = crate::support::unique_run_id();
    let payload = serde_json::json!({
        "payload": {
            "ts": "2026-04-05T12:00:00Z",
            "run_id": run_id,
            "event": "run.completed",
        }
    });
    let attach_mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/attach")
            .query_param("run_id", run_id.as_str());
        then.status(200)
            .header("Content-Type", "text/event-stream")
            .body(format!("data: {payload}\n\n"));
    });

    let output = context
        .command()
        .args([
            "--json",
            "system",
            "events",
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--run-id",
            &run_id,
        ])
        .output()
        .expect("command should run");

    assert!(output.status.success(), "system events failed");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(stdout.trim(), payload.to_string());
    attach_mock.assert();
}
