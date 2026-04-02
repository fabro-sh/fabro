use std::sync::Arc;

use fabro_store::Store;
use fabro_test::{fabro_snapshot, test_context};
use fabro_types::RunId;
use object_store::local::LocalFileSystem;

use super::support::{git_filters, setup_git_backed_changed_run, setup_git_backed_noop_run};

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
    cmd.args(["diff", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Show the diff of changes from a workflow run

    Usage: fabro diff [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID or prefix

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --node <NODE>                Show diff for a specific node
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --stat                       Show diffstat instead of full patch (live diffs only)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --shortstat                  Show only files-changed/insertions/deletions summary (live diffs only)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn diff_completed_run_without_changes_reports_no_patch() {
    let context = test_context!();
    let setup = setup_git_backed_noop_run(&context);
    let mut cmd = context.command();
    cmd.args(["diff", &setup.run.run_id]);

    fabro_snapshot!(git_filters(&context), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Run completed but no final.patch exists — the run may not have produced any changes
    ");
}

#[test]
fn diff_missing_node_diff_reports_helpful_error() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let mut cmd = context.command();
    cmd.args(["diff", &setup.run.run_id, "--node", "missing"]);

    fabro_snapshot!(git_filters(&context), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: No diff found for node 'missing' — check the node ID and try again
    ");
}

#[test]
fn diff_completed_run_with_changes_prints_patch() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let mut cmd = context.command();
    cmd.args(["diff", &setup.run.run_id]);

    fabro_snapshot!(git_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    diff --git a/story.txt b/story.txt
    index [SHA]..[SHA] 100644
    --- a/story.txt
    +++ b/story.txt
    @@ -1 +1,3 @@
     line 1
    +line 2
    +line 3
    ----- stderr -----
    ");
}

#[test]
fn diff_completed_run_reads_store_final_patch_without_disk_file() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let run_id: RunId = setup.run.run_id.parse().unwrap();
    let patch = with_runtime(|runtime| {
        runtime.block_on(async {
            let store = build_store(&context.storage_dir);
            let run_store = store.open_run(&run_id).await.unwrap();
            run_store.get_final_patch().await.unwrap().unwrap()
        })
    });
    let _ = std::fs::remove_file(setup.run.run_dir.join("final.patch"));

    with_runtime(|runtime| {
        runtime.block_on(async {
            let store = build_store(&context.storage_dir);
            let run_store = store.open_run(&run_id).await.unwrap();
            run_store.put_final_patch(&patch).await.unwrap();
        });
    });

    let mut cmd = context.command();
    cmd.args(["diff", &setup.run.run_id]);

    fabro_snapshot!(git_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    diff --git a/story.txt b/story.txt
    index [SHA]..[SHA] 100644
    --- a/story.txt
    +++ b/story.txt
    @@ -1 +1,3 @@
     line 1
    +line 2
    +line 3
    ----- stderr -----
    ");
}

#[test]
fn diff_node_outputs_specific_patch() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let mut cmd = context.command();
    cmd.args(["diff", &setup.run.run_id, "--node", "step_one"]);

    fabro_snapshot!(git_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    diff --git a/story.txt b/story.txt
    index [SHA]..[SHA] 100644
    --- a/story.txt
    +++ b/story.txt
    @@ -1 +1,2 @@
     line 1
    +line 2
    ----- stderr -----
    ");
}

#[test]
fn diff_node_reads_store_patch_without_disk_file() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let run_id: RunId = setup.run.run_id.parse().unwrap();
    let patch = with_runtime(|runtime| {
        runtime.block_on(async {
            let store = build_store(&context.storage_dir);
            let run_store = store.open_run(&run_id).await.unwrap();
            run_store
                .get_node(&fabro_store::NodeVisitRef {
                    node_id: "step_one",
                    visit: 1,
                })
                .await
                .unwrap()
                .diff
                .unwrap()
        })
    });
    let _ = std::fs::remove_file(setup.run.run_dir.join("nodes/step_one/diff.patch"));

    with_runtime(|runtime| {
        runtime.block_on(async {
            let store = build_store(&context.storage_dir);
            let run_store = store.open_run(&run_id).await.unwrap();
            run_store
                .put_node_diff(
                    &fabro_store::NodeVisitRef {
                        node_id: "step_one",
                        visit: 1,
                    },
                    &patch,
                )
                .await
                .unwrap();
        });
    });

    let mut cmd = context.command();
    cmd.args(["diff", &setup.run.run_id, "--node", "step_one"]);

    fabro_snapshot!(git_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    diff --git a/story.txt b/story.txt
    index [SHA]..[SHA] 100644
    --- a/story.txt
    +++ b/story.txt
    @@ -1 +1,2 @@
     line 1
    +line 2
    ----- stderr -----
    ");
}
