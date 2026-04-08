use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_completed_fast_dry_run;

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
