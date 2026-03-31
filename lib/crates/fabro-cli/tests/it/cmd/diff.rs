use fabro_test::{fabro_snapshot, test_context};

use super::support::{git_filters, setup_git_backed_changed_run, setup_git_backed_noop_run};

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
      > No such file or directory (os error 2)
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
