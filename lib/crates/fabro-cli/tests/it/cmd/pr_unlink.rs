use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

use super::support::mock_resolved_run;
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["pr", "unlink", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Unlink the pull request associated with a run

    Usage: fabro pr unlink [OPTIONS] <RUN_ID>

    Arguments:
      <RUN_ID>  Run ID or prefix

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
fn pr_unlink_uses_server_endpoint_and_prints_removed_url() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();

    let resolve_mock = mock_resolved_run(&server, "nightly-build", &run_id);
    let unlink_mock = server.mock(|when, then| {
        when.method("DELETE")
            .path(format!("/api/v1/runs/{run_id}/pull_request"));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "provider": "external",
                "html_url": "https://gitlab.com/acme/widgets/-/merge_requests/42",
                "title": "Review deployment chart"
            }));
    });

    let mut cmd = context.command();
    cmd.args([
        "pr",
        "unlink",
        "--server",
        &server.base_url(),
        "nightly-build",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Unlinked pull request: https://gitlab.com/acme/widgets/-/merge_requests/42
    ----- stderr -----
    ");

    resolve_mock.assert();
    unlink_mock.assert();
}
