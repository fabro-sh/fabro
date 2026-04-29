#![allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]

use fabro_test::{fabro_snapshot, test_context};
use fabro_types::run_event::PullRequestCreatedProps;
use fabro_types::{EventBody, RunEvent, RunId};
use httpmock::MockServer;

use super::support::{mock_resolved_run, server_endpoint, setup_seeded_completed_dry_run};
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["pr", "view", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    View pull request details

    Usage: fabro pr view [OPTIONS] <RUN_ID>

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
fn pr_view_missing_pull_request_json_errors() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["pr", "view", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × No pull request found in store. Create one first with: fabro pr create [ULID]
    ");
}

#[test]
fn pr_view_reads_pull_request_from_store_without_pull_request_json() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let run_id: RunId = run.run_id.parse().unwrap();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (client, base_url) =
            server_endpoint(&context.storage_dir).expect("server endpoint should exist");
        let event = RunEvent {
            id: ulid::Ulid::new().to_string(),
            ts: chrono::Utc::now(),
            run_id,
            node_id: None,
            node_label: None,
            stage_id: None,
            parallel_group_id: None,
            parallel_branch_id: None,
            session_id: None,
            parent_session_id: None,
            tool_call_id: None,
            actor: None,
            body: EventBody::PullRequestCreated(PullRequestCreatedProps {
                pr_url:      "https://github.com/fabro-sh/fabro/pull/123".to_string(),
                pr_number:   123,
                owner:       "fabro-sh".to_string(),
                repo:        "fabro".to_string(),
                base_branch: "main".to_string(),
                head_branch: "fabro/run/demo".to_string(),
                title:       "Map the constellations".to_string(),
                draft:       false,
            }),
        };
        client
            .post(format!("{base_url}/api/v1/runs/{run_id}/events"))
            .json(&event)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });

    let mut cmd = context.command();
    cmd.args(["pr", "view", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × GitHub integration unavailable on server.
    ");
}

#[test]
fn pr_view_uses_server_pull_request_endpoint_and_renders_merged_state() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();

    let resolve_mock = mock_resolved_run(&server, "nightly-build", &run_id);
    let detail_mock = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/pull_request"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "record": {
                        "html_url": "https://github.com/fabro-sh/fabro/pull/123",
                        "number": 123,
                        "owner": "fabro-sh",
                        "repo": "fabro",
                        "base_branch": "main",
                        "head_branch": "fabro/run/demo",
                        "title": "Map the constellations"
                    },
                    "number": 123,
                    "title": "Map the constellations",
                    "body": "Detailed description",
                    "state": "closed",
                    "draft": false,
                    "merged": true,
                    "merged_at": "2026-04-06T12:30:00Z",
                    "mergeable": false,
                    "additions": 10,
                    "deletions": 3,
                    "changed_files": 2,
                    "html_url": "https://github.com/fabro-sh/fabro/pull/123",
                    "user": {
                        "login": "testuser"
                    },
                    "head": {
                        "ref": "fabro/run/demo"
                    },
                    "base": {
                        "ref": "main"
                    },
                    "created_at": "2026-04-05T12:00:00Z",
                    "updated_at": "2026-04-06T12:30:00Z"
                })
                .to_string(),
            );
    });

    let mut cmd = context.command();
    cmd.args([
        "pr",
        "view",
        "--server",
        &server.base_url(),
        "nightly-build",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    #123 Map the constellations
    State:   merged
    URL:     https://github.com/fabro-sh/fabro/pull/123
    Branch:  fabro/run/demo -> main
    Author:  testuser
    Changes: +10 -3 (2 files)

    Detailed description
    ----- stderr -----
    ");

    resolve_mock.assert();
    detail_mock.assert();
}
