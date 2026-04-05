#![allow(clippy::absolute_paths)]

use fabro_test::{fabro_snapshot, test_context};
use fabro_types::run_event::PullRequestCreatedProps;
use fabro_types::{EventBody, RunEvent, RunId};

use super::support::setup_completed_fast_dry_run;

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
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn pr_view_missing_pull_request_json_errors() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["pr", "view", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: No pull request found in store. Create one first with: fabro pr create [ULID]
    ");
}

#[test]
fn pr_view_reads_pull_request_from_store_without_pull_request_json() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let run_id: RunId = run.run_id.parse().unwrap();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let client = reqwest::ClientBuilder::new()
            .unix_socket(context.storage_dir.join("fabro.sock"))
            .no_proxy()
            .build()
            .unwrap();
        let event = RunEvent {
            id: ulid::Ulid::new().to_string(),
            ts: chrono::Utc::now(),
            run_id,
            node_id: None,
            node_label: None,
            session_id: None,
            parent_session_id: None,
            body: EventBody::PullRequestCreated(PullRequestCreatedProps {
                pr_url: "https://github.com/fabro-sh/fabro/pull/123".to_string(),
                pr_number: 123,
                owner: "fabro-sh".to_string(),
                repo: "fabro".to_string(),
                base_branch: "main".to_string(),
                head_branch: "fabro/run/demo".to_string(),
                title: "Map the constellations".to_string(),
                draft: false,
            }),
        };
        client
            .post(format!("http://fabro/api/v1/runs/{run_id}/events"))
            .json(&event)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });

    let pr_path = run.run_dir.join("pull_request.json");
    if pr_path.exists() {
        std::fs::remove_file(pr_path).unwrap();
    }

    let mut cmd = context.command();
    cmd.args(["pr", "view", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id
    ");
}
