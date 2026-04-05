use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_completed_fast_dry_run;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["artifact", "cp", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copy artifacts from a workflow run

    Usage: fabro artifact cp [OPTIONS] <SOURCE> [DEST]

    Arguments:
      <SOURCE>  Source: RUN_ID (all artifacts) or RUN_ID:path (specific artifact)
      [DEST]    Destination directory (defaults to current directory) [default: .]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --node <NODE>                Filter to artifacts from a specific node
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --retry <RETRY>              Filter to artifacts from a specific retry attempt
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --tree                       Preserve {node_slug}/retry_{N}/ directory structure
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn artifact_cp_empty_run_reports_no_artifacts() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let dest = context.temp_dir.join("artifact-dest");
    let mut cmd = context.command();
    cmd.args(["artifact", "cp", &run.run_id, dest.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: No artifacts found for this run
    ");
}
