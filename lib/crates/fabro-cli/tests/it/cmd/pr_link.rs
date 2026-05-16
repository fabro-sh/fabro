use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

use super::support::mock_resolved_run;
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["pr", "link", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Link or replace the GitHub pull request associated with a run

    Usage: fabro pr link [OPTIONS] <RUN_ID> <URL>

    Arguments:
      <RUN_ID>  Run ID or prefix
      <URL>     GitHub pull request URL to associate with the run

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
fn pr_link_uses_server_endpoint_and_prints_linked_record() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();

    let resolve_mock = mock_resolved_run(&server, "nightly-build", &run_id);
    let link_mock = server.mock(|when, then| {
        when.method("PUT")
            .path(format!("/api/v1/runs/{run_id}/pull_request"))
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "html_url": "https://github.com/acme/widgets/pull/42"
            }));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "owner": "acme",
                "repo": "widgets",
                "number": 42,
                "html_url": "https://github.com/acme/widgets/pull/42"
            }));
    });

    let mut cmd = context.command();
    cmd.args([
        "pr",
        "link",
        "--server",
        &server.base_url(),
        "nightly-build",
        "https://github.com/acme/widgets/pull/42",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Linked pull request: https://github.com/acme/widgets/pull/42 (github #42)
    ----- stderr -----
    ");

    resolve_mock.assert();
    link_mock.assert();
}

#[test]
fn pr_link_skips_resolve_endpoint_for_full_run_id() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();

    let link_mock = server.mock(|when, then| {
        when.method("PUT")
            .path(format!("/api/v1/runs/{run_id}/pull_request"))
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "html_url": "https://github.com/acme/widgets/pull/42"
            }));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "owner": "acme",
                "repo": "widgets",
                "number": 42,
                "html_url": "https://github.com/acme/widgets/pull/42"
            }));
    });

    let mut cmd = context.command();
    cmd.args([
        "pr",
        "link",
        "--server",
        &server.base_url(),
        &run_id,
        "https://github.com/acme/widgets/pull/42",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Linked pull request: https://github.com/acme/widgets/pull/42 (github #42)
    ----- stderr -----
    ");

    link_mock.assert();
}
