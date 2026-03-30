use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_completed_dry_run;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["system", "df", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Show disk usage

    Usage: fabro system df [OPTIONS]

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -v, --verbose                    Show per-run breakdown
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn system_df_summarizes_runs_logs_and_databases() {
    let context = test_context!();
    setup_completed_dry_run(&context);
    std::fs::create_dir_all(context.storage_dir.join("logs")).unwrap();
    std::fs::write(context.storage_dir.join("logs/cli.log"), b"log line\n").unwrap();
    std::fs::write(context.storage_dir.join("fabro.db"), b"db").unwrap();
    std::fs::write(context.storage_dir.join("fabro.db-wal"), b"wal").unwrap();

    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?\s(?:[KMGT]?B|B)\b".to_string(),
        "[SIZE]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["system", "df"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    TYPE       COUNT  ACTIVE     SIZE     RECLAIMABLE 
     Runs           1       0  [SIZE]  [SIZE] (100%) 
     Logs           1       -      [SIZE]      [SIZE] (100%) 
     Databases      2       -      [SIZE]        [SIZE] (0%)

    Data directory: [STORAGE_DIR]
    ----- stderr -----
    ");
}

#[test]
fn system_df_verbose_lists_runs_with_reclaimable_marker() {
    let context = test_context!();
    setup_completed_dry_run(&context);

    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?\s(?:[KMGT]?B|B)\b".to_string(),
        "[SIZE]".to_string(),
    ));
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[RUN_PREFIX]".to_string(),
    ));
    filters.push((r"\b\d+[mhd]\b".to_string(), "[AGE]".to_string()));

    let mut cmd = context.command();
    cmd.args(["system", "df", "-v"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    TYPE       COUNT  ACTIVE     SIZE     RECLAIMABLE 
     Runs           1       0  [SIZE]  [SIZE] (100%) 
     Logs           0       -      [SIZE]        [SIZE] (0%) 
     Databases      0       -      [SIZE]        [SIZE] (0%)

    Data directory: [STORAGE_DIR]

    RUN ID        WORKFLOW  STATUS     AGE       SIZE 
     [RUN_PREFIX]  Simple    succeeded   [AGE]  [SIZE] *

    * = reclaimable
    ----- stderr -----
    ");
}
