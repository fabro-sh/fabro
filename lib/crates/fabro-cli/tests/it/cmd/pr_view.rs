use std::sync::Arc;

use fabro_test::{fabro_snapshot, test_context};
use fabro_types::RunId;
use fabro_workflow::event::{Event, append_event};
use object_store::local::LocalFileSystem;

use super::support::setup_completed_fast_dry_run;

fn with_runtime<T>(f: impl FnOnce(&tokio::runtime::Runtime) -> T) -> T {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    f(&runtime)
}

fn build_store(storage_dir: &std::path::Path) -> Arc<fabro_store::SlateStore> {
    let store_path = storage_dir.join("store");
    std::fs::create_dir_all(&store_path).unwrap();
    let object_store = Arc::new(LocalFileSystem::new_with_prefix(&store_path).unwrap());
    Arc::new(fabro_store::SlateStore::new(
        object_store,
        "",
        std::time::Duration::from_millis(1),
    ))
}

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
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
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

    with_runtime(|runtime| {
        runtime.block_on(async {
            let store = build_store(&context.storage_dir);
            let run_store = store.open_run(&run_id).await.unwrap();
            append_event(
                &run_store,
                &run_id,
                &Event::PullRequestCreated {
                    pr_url: "https://github.com/fabro-sh/fabro/pull/123".to_string(),
                    pr_number: 123,
                    owner: "fabro-sh".to_string(),
                    repo: "fabro".to_string(),
                    base_branch: "main".to_string(),
                    head_branch: "fabro/run/demo".to_string(),
                    title: "Map the constellations".to_string(),
                    draft: false,
                },
            )
            .await
            .unwrap();
        });
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
