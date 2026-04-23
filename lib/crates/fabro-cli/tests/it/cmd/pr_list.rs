#![allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]

use fabro_test::{fabro_snapshot, test_context};
use fabro_types::run_event::PullRequestCreatedProps;
use fabro_types::{EventBody, RunEvent, RunId};
use httpmock::MockServer;

use super::support::{server_endpoint, setup_completed_fast_dry_run};
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["pr", "list", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List pull requests from workflow runs

    Usage: fabro pr list [OPTIONS]

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --all               Show all PRs (including closed/merged), not just open
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

// Seed a PR event against this test's own run so the store is guaranteed to
// have at least one entry; `fabro pr list` then must ask the server for live
// PR detail and fail if the server lacks GitHub credentials.
#[test]
fn pr_list_missing_github_credentials_errors() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
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
    cmd.args(["pr", "list"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: GitHub integration unavailable on server.
    ");
}

#[test]
fn pr_list_uses_server_pull_request_endpoint_and_skips_runs_without_records() {
    let context = test_context!();
    let server = MockServer::start();
    let pr_run_id = unique_run_id();
    let no_pr_run_id = unique_run_id();

    let list_mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs")
            .query_param("page[limit]", "100")
            .query_param("page[offset]", "0")
            .query_param("include_archived", "true");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [
                        {
                            "run_id": pr_run_id,
                            "workflow_name": "Nightly Build",
                            "workflow_slug": "nightly-build",
                            "goal": "Nightly run",
                            "title": "Nightly run",
                            "labels": {},
                            "host_repo_path": null,
                            "repository": { "name": "unknown" },
                            "start_time": "2026-04-05T12:00:00Z",
                            "created_at": "2026-04-05T12:00:00Z",
                            "status": {
                                "kind": "succeeded",
                                "reason": "completed"
                            },
                            "pending_control": null,
                            "duration_ms": 123,
                            "elapsed_secs": 0,
                            "total_usd_micros": null
                        },
                        {
                            "run_id": no_pr_run_id,
                            "workflow_name": "Docs",
                            "workflow_slug": "docs",
                            "goal": "Docs run",
                            "title": "Docs run",
                            "labels": {},
                            "host_repo_path": null,
                            "repository": { "name": "unknown" },
                            "start_time": "2026-04-04T12:00:00Z",
                            "created_at": "2026-04-04T12:00:00Z",
                            "status": {
                                "kind": "succeeded",
                                "reason": "completed"
                            },
                            "pending_control": null,
                            "duration_ms": 123,
                            "elapsed_secs": 0,
                            "total_usd_micros": null
                        }
                    ],
                    "meta": {
                        "has_more": false
                    }
                })
                .to_string(),
            );
    });
    let pr_state_mock = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{pr_run_id}/state"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "nodes": {},
                    "pull_request": {
                        "html_url": "https://github.com/fabro-sh/fabro/pull/123",
                        "number": 123,
                        "owner": "fabro-sh",
                        "repo": "fabro",
                        "base_branch": "main",
                        "head_branch": "fabro/run/demo",
                        "title": "Map the constellations"
                    }
                })
                .to_string(),
            );
    });
    let no_pr_state_mock = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{no_pr_run_id}/state"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(serde_json::json!({ "nodes": {}, "pull_request": null }).to_string());
    });
    let detail_mock = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{pr_run_id}/pull_request"));
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
                    "body": null,
                    "state": "open",
                    "draft": false,
                    "merged": false,
                    "merged_at": null,
                    "mergeable": true,
                    "additions": 10,
                    "deletions": 3,
                    "changed_files": 2,
                    "html_url": "https://github.com/fabro-sh/fabro/pull/123",
                    "user": {
                        "login": "testuser"
                    },
                    "head": {
                        "ref_name": "fabro/run/demo"
                    },
                    "base": {
                        "ref_name": "main"
                    },
                    "created_at": "2026-04-05T12:00:00Z",
                    "updated_at": "2026-04-05T12:00:00Z"
                })
                .to_string(),
            );
    });

    let mut cmd = context.command();
    cmd.args(["pr", "list", "--json", "--server", &server.base_url()]);

    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [
      {
        "run_id": "[ULID]",
        "number": 123,
        "state": "open",
        "merged": false,
        "title": "Map the constellations",
        "url": "https://github.com/fabro-sh/fabro/pull/123"
      }
    ]
    ----- stderr -----
    "#);

    list_mock.assert();
    pr_state_mock.assert();
    no_pr_state_mock.assert();
    detail_mock.assert();
}
