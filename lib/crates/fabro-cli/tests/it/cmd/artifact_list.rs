use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_seeded_completed_dry_run;

#[test]
fn artifact_list_empty_run_reports_no_artifacts() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["artifact", "list", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    No artifacts found for this run.
    ----- stderr -----
    ");
}
