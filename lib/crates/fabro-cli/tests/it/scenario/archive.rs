use fabro_test::test_context;
use serde_json::Value;

use crate::cmd::support::setup_completed_fast_dry_run;

fn ps_runs(context: &fabro_test::TestContext, include_archived: bool) -> Vec<Value> {
    let label = context.test_case_label();
    let mut cmd = context.ps();
    if include_archived {
        cmd.arg("-a");
    }
    cmd.args(["--json", "--label", &label]);
    let output = cmd.output().expect("ps should execute");
    assert!(
        output.status.success(),
        "ps failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("ps JSON should parse")
}

#[test]
fn archive_lifecycle_end_to_end() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

    let visible = ps_runs(&context, true);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0]["run_id"], run.run_id);
    assert_eq!(visible[0]["status"]["kind"], "succeeded");
    assert_eq!(visible[0]["status"]["reason"], "completed");

    let archive = context
        .command()
        .args(["archive", &run.run_id])
        .output()
        .expect("archive should execute");
    assert!(
        archive.status.success(),
        "archive failed\nstderr:\n{}",
        String::from_utf8_lossy(&archive.stderr)
    );

    let default_visible = ps_runs(&context, false);
    assert!(
        default_visible.is_empty(),
        "default ps should hide archived, got {default_visible:?}"
    );

    let with_archived = ps_runs(&context, true);
    assert_eq!(with_archived.len(), 1);
    assert_eq!(with_archived[0]["run_id"], run.run_id);
    assert_eq!(with_archived[0]["status"]["kind"], "archived");
    assert_eq!(with_archived[0]["status"]["prior"]["kind"], "succeeded");
    assert_eq!(with_archived[0]["status"]["prior"]["reason"], "completed");

    let unarchive = context
        .command()
        .args(["unarchive", &run.run_id])
        .output()
        .expect("unarchive should execute");
    assert!(
        unarchive.status.success(),
        "unarchive failed\nstderr:\n{}",
        String::from_utf8_lossy(&unarchive.stderr)
    );
    let restored = ps_runs(&context, true);
    assert_eq!(restored[0]["status"]["kind"], "succeeded");
    assert_eq!(restored[0]["status"]["reason"], "completed");

    // `rm` must remain available on archived runs — archive and delete are
    // orthogonal per the plan's Scope Boundaries.
    context
        .command()
        .args(["archive", &run.run_id])
        .output()
        .expect("re-archive should execute");
    let rm = context
        .command()
        .args(["rm", &run.run_id])
        .output()
        .expect("rm should execute");
    assert!(
        rm.status.success(),
        "rm on archived run should succeed\nstderr:\n{}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(!run.run_dir.exists(), "run directory should be deleted");
    let after_delete = ps_runs(&context, true);
    assert!(
        after_delete.is_empty(),
        "run should be gone after rm, got {after_delete:?}"
    );
}
