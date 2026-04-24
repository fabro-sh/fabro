use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

use super::support::mock_resolved_run;
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["pr", "merge", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Merge a pull request

    Usage: fabro pr merge [OPTIONS] <RUN_ID>

    Arguments:
      <RUN_ID>  Run ID or prefix

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --method <METHOD>   Merge method: merge, squash, or rebase [default: squash] [possible values: merge, squash, rebase]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn pr_merge_uses_server_endpoint() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();

    let resolve_mock = mock_resolved_run(&server, "nightly-build", &run_id);
    let merge_mock = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/pull_request/merge"))
            .json_body(serde_json::json!({ "method": "squash" }));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "number": 123,
                    "html_url": "https://github.com/fabro-sh/fabro/pull/123",
                    "method": "squash"
                })
                .to_string(),
            );
    });

    let mut cmd = context.command();
    cmd.args([
        "pr",
        "merge",
        "--server",
        &server.base_url(),
        "nightly-build",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Merged #123 (https://github.com/fabro-sh/fabro/pull/123)
    ----- stderr -----
    ");

    resolve_mock.assert();
    merge_mock.assert();
}
