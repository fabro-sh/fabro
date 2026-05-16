use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::Value;

use super::support::{mock_resolved_run, remote_run_summary_json};
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["parent", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Manage run parent links

    Usage: fabro parent [OPTIONS] <COMMAND>

    Commands:
      link    Link or replace a run's orchestration parent
      unlink  Unlink a run from its orchestration parent
      help    Print this message or the help of the given subcommand(s)

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
fn parent_link_resolves_selectors_calls_endpoint_and_prints_link() {
    let context = test_context!();
    let server = MockServer::start();
    let child_id = unique_run_id();
    let parent_id = unique_run_id();

    let child_resolve = mock_resolved_run(&server, "child-build", &child_id);
    let parent_resolve = mock_resolved_run(&server, "parent-build", &parent_id);
    let link_mock = server.mock(|when, then| {
        when.method("PUT")
            .path(format!("/api/v1/runs/{child_id}/parent"))
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "parent_id": parent_id
            }));
        let mut summary = remote_run_summary_json(
            &child_id,
            "Nightly Build",
            "nightly-build",
            "Nightly run",
            &serde_json::json!({
                "kind": "succeeded",
                "reason": "completed"
            }),
            "2026-04-05T12:00:00Z",
        );
        summary["parent_id"] = serde_json::json!(parent_id);
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(summary);
    });

    let mut cmd = context.command();
    cmd.args([
        "parent",
        "link",
        "--server",
        &server.base_url(),
        "child-build",
        "parent-build",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Linked parent: [ULID] -> [ULID]
    ----- stderr -----
    ");

    child_resolve.assert();
    parent_resolve.assert();
    link_mock.assert();
}

#[test]
fn parent_link_json_prints_updated_run_summary() {
    let context = test_context!();
    let server = MockServer::start();
    let child_id = unique_run_id();
    let parent_id = unique_run_id();

    let child_resolve = mock_resolved_run(&server, "child-build", &child_id);
    let parent_resolve = mock_resolved_run(&server, "parent-build", &parent_id);
    let link_mock = server.mock(|when, then| {
        when.method("PUT")
            .path(format!("/api/v1/runs/{child_id}/parent"))
            .json_body(serde_json::json!({
                "parent_id": parent_id
            }));
        let mut summary = remote_run_summary_json(
            &child_id,
            "Nightly Build",
            "nightly-build",
            "Nightly run",
            &serde_json::json!({
                "kind": "succeeded",
                "reason": "completed"
            }),
            "2026-04-05T12:00:00Z",
        );
        summary["parent_id"] = serde_json::json!(parent_id);
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(summary);
    });

    let output = context
        .command()
        .args([
            "--json",
            "parent",
            "link",
            "--server",
            &server.base_url(),
            "child-build",
            "parent-build",
        ])
        .output()
        .expect("parent link should execute");

    assert!(
        output.status.success(),
        "parent link failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value = serde_json::from_slice(&output.stdout).expect("JSON should parse");
    assert_eq!(summary["id"], child_id);
    assert_eq!(summary["parent_id"], parent_id);

    child_resolve.assert();
    parent_resolve.assert();
    link_mock.assert();
}

#[test]
fn parent_unlink_resolves_selector_calls_endpoint_and_prints_unlinked_child() {
    let context = test_context!();
    let server = MockServer::start();
    let child_id = unique_run_id();

    let child_resolve = mock_resolved_run(&server, "child-build", &child_id);
    let unlink_mock = server.mock(|when, then| {
        when.method("DELETE")
            .path(format!("/api/v1/runs/{child_id}/parent"));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &child_id,
                "Nightly Build",
                "nightly-build",
                "Nightly run",
                &serde_json::json!({
                    "kind": "succeeded",
                    "reason": "completed"
                }),
                "2026-04-05T12:00:00Z",
            ));
    });

    let mut cmd = context.command();
    cmd.args([
        "parent",
        "unlink",
        "--server",
        &server.base_url(),
        "child-build",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Unlinked parent: [ULID]
    ----- stderr -----
    ");

    child_resolve.assert();
    unlink_mock.assert();
}
