use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

use crate::support::{LightweightCli, unique_run_id};

fn live_run_state_response() -> serde_json::Value {
    serde_json::json!({
        "run": null,
        "graph_source": null,
        "start": null,
        "status": {
            "status": "running",
            "reason": null,
            "updated_at": "2026-04-05T12:00:01Z"
        },
        "checkpoint": null,
        "checkpoints": [],
        "conclusion": null,
        "retro": null,
        "retro_prompt": null,
        "retro_response": null,
        "sandbox": null,
        "final_patch": null,
        "pull_request": null,
        "nodes": {}
    })
}

fn run_sse_body(run_id: &str) -> String {
    let completed = serde_json::json!({
        "seq": 2,
        "event": "run.completed",
        "id": "evt-run-completed",
        "run_id": run_id,
        "ts": "2026-04-05T12:00:01Z",
        "properties": {
            "duration_ms": 12,
            "artifact_count": 0,
            "status": "success"
        }
    });

    format!("data: {completed}\n\n")
}

#[test]
fn help_smoke_covers_high_cost_commands() {
    let cli = LightweightCli::new();

    let mut artifact = cli.command();
    artifact.args(["artifact", "--help"]);
    fabro_snapshot!(artifact, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Inspect and copy run artifacts (screenshots, reports, traces)

    Usage: fabro artifact [OPTIONS] <COMMAND>

    Commands:
      list  List artifacts for a workflow run
      cp    Copy artifacts from a workflow run
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

    let mut artifact_list = cli.command();
    artifact_list.args(["artifact", "list", "--help"]);
    fabro_snapshot!(artifact_list, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List artifacts for a workflow run

    Usage: fabro artifact list [OPTIONS] <RUN_ID>

    Arguments:
      <RUN_ID>  Run ID (or prefix)

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --node <NODE>       Filter to artifacts from a specific node
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --retry <RETRY>     Filter to artifacts from a specific retry attempt
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");

    let mut artifact_cp = cli.command();
    artifact_cp.args(["artifact", "cp", "--help"]);
    fabro_snapshot!(artifact_cp, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copy artifacts from a workflow run

    Usage: fabro artifact cp [OPTIONS] <SOURCE> [DEST]

    Arguments:
      <SOURCE>  Source: RUN_ID (all artifacts) or RUN_ID:path (specific artifact)
      [DEST]    Destination directory (defaults to current directory) [default: .]

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --node <NODE>       Filter to artifacts from a specific node
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --retry <RETRY>     Filter to artifacts from a specific retry attempt
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --tree              Preserve {node_slug}/retry_{N}/ directory structure
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");

    let mut settings = cli.command();
    settings.args(["settings", "--help"]);
    fabro_snapshot!(settings, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Inspect effective settings

    Usage: fabro settings [OPTIONS] [WORKFLOW]

    Arguments:
      [WORKFLOW]  Optional workflow name, .fabro path, or .toml run config to overlay

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --local             Show only locally resolved settings and skip the server call
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");

    let mut attach = cli.command();
    attach.args(["attach", "--help"]);
    fabro_snapshot!(attach, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Attach to a running or finished workflow run

    Usage: fabro attach [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn completion_smoke_covers_help_and_generation() {
    let cli = LightweightCli::new();

    let mut help = cli.command();
    help.args(["completion", "--help"]);
    fabro_snapshot!(help, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Generate shell completions

    Usage: fabro completion [OPTIONS] <SHELL>

    Arguments:
      <SHELL>  Shell to generate completions for [possible values: bash, elvish, fish, powershell, zsh]

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");

    let mut zsh = cli.command();
    zsh.args(["completion", "zsh"]);
    zsh.assert().success();

    let mut fish = cli.command();
    fish.args(["completion", "fish"]);
    fish.assert().success();
}

#[test]
fn attach_smoke_covers_arg_validation_and_remote_server_behaviors() {
    let context = test_context!();

    let mut missing_arg = context.command();
    missing_arg.arg("attach");
    fabro_snapshot!(context.filters(), missing_arg, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUN>

    Usage: fabro attach --no-upgrade-check <RUN>

    For more information, try '--help'.
    ");

    let success_server = MockServer::start();
    let success_run_id = unique_run_id();
    let list_mock = success_server.mock(|when, then| {
        when.method("GET").path("/api/v1/runs");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!([
                    {
                        "run_id": success_run_id,
                        "workflow_name": "Remote Workflow",
                        "workflow_slug": "remote-workflow",
                        "goal": "Remote output",
                        "labels": {},
                        "host_repo_path": null,
                        "start_time": "2026-04-05T12:00:00Z",
                        "status": "running",
                        "status_reason": null,
                        "duration_ms": 12,
                        "total_usd_micros": null
                    }
                ])
                .to_string(),
            );
    });
    success_server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{success_run_id}/events"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [{
                        "seq": 1,
                        "event": "run.running",
                        "id": "evt-run-running",
                        "run_id": success_run_id,
                        "ts": "2026-04-05T12:00:00Z",
                        "properties": {}
                    }],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    success_server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{success_run_id}/state"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(live_run_state_response().to_string());
    });
    success_server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{success_run_id}/questions"))
            .query_param("page[limit]", "100")
            .query_param("page[offset]", "0");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"data":[],"meta":{"has_more":false}}"#);
    });
    let attach_mock = success_server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{success_run_id}/attach"))
            .query_param("since_seq", "2");
        then.status(200)
            .header("Content-Type", "text/event-stream")
            .body(run_sse_body(success_run_id.as_str()));
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "_version = 1\n\n[cli.target]\ntype = \"http\"\nurl = \"{}/api/v1\"\n",
            success_server.base_url()
        ),
    );

    let success_output = context
        .command()
        .args(["--json", "attach", &success_run_id])
        .output()
        .expect("attach should execute");

    assert!(
        success_output.status.success(),
        "attach failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&success_output.stdout),
        String::from_utf8_lossy(&success_output.stderr)
    );
    list_mock.assert();
    attach_mock.assert();
    let success_stdout = String::from_utf8(success_output.stdout).expect("stdout should be UTF-8");
    assert!(
        success_stdout.contains("\"event\":\"run.completed\""),
        "{success_stdout}"
    );

    let eof_server = MockServer::start();
    let eof_run_id = unique_run_id();

    eof_server.mock(|when, then| {
        when.method("GET").path("/api/v1/runs");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!([
                    {
                        "run_id": eof_run_id,
                        "workflow_name": "Remote Workflow",
                        "workflow_slug": "remote-workflow",
                        "goal": "Remote output",
                        "labels": {},
                        "host_repo_path": null,
                        "start_time": "2026-04-05T12:00:00Z",
                        "status": "running",
                        "status_reason": null,
                        "duration_ms": 12,
                        "total_cost": null
                    }
                ])
                .to_string(),
            );
    });
    eof_server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{eof_run_id}/events"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [{
                        "seq": 1,
                        "event": "run.running",
                        "id": "evt-run-running",
                        "run_id": eof_run_id,
                        "ts": "2026-04-05T12:00:00Z",
                        "properties": {}
                    }],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    eof_server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{eof_run_id}/state"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(live_run_state_response().to_string());
    });
    eof_server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{eof_run_id}/questions"))
            .query_param("page[limit]", "100")
            .query_param("page[offset]", "0");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"data":[],"meta":{"has_more":false}}"#);
    });
    eof_server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{eof_run_id}/attach"))
            .query_param("since_seq", "2");
        then.status(200)
            .header("Content-Type", "text/event-stream")
            .body("");
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "_version = 1\n\n[cli.target]\ntype = \"http\"\nurl = \"{}/api/v1\"\n",
            eof_server.base_url()
        ),
    );

    let eof_output = context
        .command()
        .args(["attach", &eof_run_id])
        .output()
        .expect("attach should execute");

    assert!(
        !eof_output.status.success(),
        "attach should fail on premature EOF"
    );
    let eof_stderr = String::from_utf8(eof_output.stderr).expect("stderr should be UTF-8");
    assert!(
        eof_stderr.contains("terminal run event"),
        "expected a protocol error, got:\n{eof_stderr}"
    );
}
