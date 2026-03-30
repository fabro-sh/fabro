use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["ps", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List workflow runs

    Usage: fabro ps [OPTIONS]

    Options:
          --before <BEFORE>            Only include runs started before this date (YYYY-MM-DD prefix match)
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --workflow <WORKFLOW>        Filter by workflow name (substring match)
          --label <KEY=VALUE>          Filter by label (KEY=VALUE, repeatable, AND semantics)
          --orphans                    Include orphan directories (no run.json)
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --json                       Output as JSON
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -a, --all                        Show all runs, not just running (like docker ps -a)
      -q, --quiet                      Only display run IDs
      -h, --help                       Print help
    ----- stderr -----
    ");
}
