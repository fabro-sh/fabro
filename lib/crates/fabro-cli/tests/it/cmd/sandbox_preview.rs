use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::{Value, json};

use super::support::setup_local_sandbox_run;
use crate::support::unique_run_id;

fn remote_run_summary(run_id: &str) -> serde_json::Value {
    json!({
        "run_id": run_id,
        "workflow_name": "Preview Test",
        "workflow_slug": "preview-test",
        "goal": "Preview test",
        "title": "Preview test",
        "labels": {},
        "source_directory": "/srv/repo",
        "repository": { "name": "repo" },
        "start_time": "2026-04-19T12:00:00Z",
        "created_at": "2026-04-19T12:00:00Z",
        "status": {
            "kind": "running"
        },
        "pending_control": null,
        "duration_ms": null,
        "elapsed_secs": null,
        "total_usd_micros": null
    })
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["sandbox", "preview", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Get a preview URL for a port on a run's sandbox

    Usage: fabro sandbox preview [OPTIONS] <RUN> <PORT>

    Arguments:
      <RUN>   Run ID or prefix
      <PORT>  Port number

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --signed            Generate a signed URL (embeds auth token, no headers needed)
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --ttl <TTL>         Signed URL expiry in seconds (default 3600, requires --signed) [default: 3600]
          --open              Open URL in browser (implies --signed)
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn sandbox_preview_rejects_non_daytona_run() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);
    let mut cmd = context.preview();
    cmd.args([&setup.run.run_id, "3000"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × Sandbox provider does not support this capability.
    ");
}

#[test]
fn sandbox_preview_open_is_suppressed_by_json_output_format_from_home_config() {
    let context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[cli.output]\nformat = \"json\"\n",
    );
    let server = MockServer::start();
    let run_id = unique_run_id();
    let resolve_run = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", run_id.as_str());
        then.status(200)
            .header("content-type", "application/json")
            .body(remote_run_summary(&run_id).to_string());
    });
    let preview = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/preview"))
            .json_body(json!({
                "port": 3000,
                "expires_in_secs": 3600,
                "signed": true,
            }));
        then.status(201)
            .header("content-type", "application/json")
            .body(json!({ "url": "https://preview.example.test/app" }).to_string());
    });

    let mut cmd = context.preview();
    cmd.args([
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "--open",
        run_id.as_str(),
        "3000",
    ]);
    let output = cmd.output().expect("command should run");

    assert!(output.status.success(), "sandbox preview should succeed");
    let value: Value = serde_json::from_slice(&output.stdout).expect("preview JSON should parse");
    assert_eq!(
        value,
        json!({
            "url": "https://preview.example.test/app"
        })
    );
    resolve_run.assert();
    preview.assert();
}
