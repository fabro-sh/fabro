#![allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]

use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

use super::support::{mock_resolved_run, setup_completed_fast_dry_run};
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["pr", "create", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Create a pull request from a completed run

    Usage: fabro pr create [OPTIONS] <RUN_ID>

    Arguments:
      <RUN_ID>  Run ID or prefix

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --model <MODEL>     LLM model for generating PR description
      -f, --force             Create PR even if the run status is not success/partial_success
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn pr_create_nongit_run_reports_missing_repo_origin() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

    let mut cmd = context.command();
    cmd.args(["pr", "create", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Run has no repo origin URL — pull request creation requires git metadata.
    ");
}

#[test]
fn pr_create_uses_server_endpoint_and_prints_url() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();

    let resolve_mock = mock_resolved_run(&server, "nightly-build", &run_id);
    let create_mock = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/pull_request"))
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "force": false
            }));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "html_url": "https://github.com/fabro-sh/fabro/pull/123",
                "number": 123,
                "owner": "fabro-sh",
                "repo": "fabro",
                "base_branch": "main",
                "head_branch": "fabro/run/demo",
                "title": "Map the constellations"
            }));
    });

    let mut cmd = context.command();
    cmd.args([
        "pr",
        "create",
        "--server",
        &server.base_url(),
        "nightly-build",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    https://github.com/fabro-sh/fabro/pull/123
    ----- stderr -----
    ");

    resolve_mock.assert();
    create_mock.assert();
}

#[test]
fn pr_create_passes_force_and_model_to_server() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();

    let resolve_mock = mock_resolved_run(&server, "nightly-build", &run_id);
    let create_mock = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/pull_request"))
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "force": true,
                "model": "gpt-5.2"
            }));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "html_url": "https://github.com/fabro-sh/fabro/pull/123",
                "number": 123,
                "owner": "fabro-sh",
                "repo": "fabro",
                "base_branch": "main",
                "head_branch": "fabro/run/demo",
                "title": "Map the constellations"
            }));
    });

    let mut cmd = context.command();
    cmd.args([
        "pr",
        "create",
        "--json",
        "--force",
        "--model",
        "gpt-5.2",
        "--server",
        &server.base_url(),
        "nightly-build",
    ]);

    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {
      "html_url": "https://github.com/fabro-sh/fabro/pull/123",
      "number": 123,
      "owner": "fabro-sh",
      "repo": "fabro",
      "base_branch": "main",
      "head_branch": "fabro/run/demo",
      "title": "Map the constellations"
    }
    ----- stderr -----
    "#);

    resolve_mock.assert();
    create_mock.assert();
}
