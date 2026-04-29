use fabro_test::{fabro_snapshot, run_and_format, test_context};
use insta::assert_snapshot;

use super::support::{
    git_filters, output_stderr as support_stderr, run_events, run_state, run_state_by_id,
    setup_seeded_git_backed_changed_run,
};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["rewind", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Rewind a workflow run to an earlier checkpoint

    Usage: fabro rewind [OPTIONS] <RUN_ID> [TARGET]

    Arguments:
      <RUN_ID>  Run ID (or unambiguous prefix)
      [TARGET]  Target checkpoint: node name, node@visit, or @ordinal (omit with --list)

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --list              Show the checkpoint timeline instead of rewinding
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn rewind_outside_git_repo_errors() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["rewind", "01ARZ3NDEKTSV4RRFFQ69G5FAW", "--list"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × No run found matching '[ULID]' (tried run ID prefix and workflow name)
    ");
}

#[test]
fn rewind_list_prints_timeline_for_completed_git_run() {
    let context = test_context!();
    let setup = setup_seeded_git_backed_changed_run(&context);
    let mut cmd = context.command();
    cmd.args(["rewind", &setup.run.run_id, "--list"]);

    fabro_snapshot!(git_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    @   Node      Details
     @1  start     (no run commit)
     @2  step_one
     @3  step_two
    ");
}

#[test]
fn rewind_target_updates_metadata_and_resume_hint() {
    let context = test_context!();
    let setup = setup_seeded_git_backed_changed_run(&context);

    let mut cmd = context.command();
    cmd.args(["rewind", &setup.run.run_id, "@2"]);

    let (snapshot, output) = run_and_format(&mut cmd, &git_filters(&context));
    assert_snapshot!(snapshot, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----

    Rewound [RUN_PREFIX]; new run [RUN_PREFIX]
    To resume: fabro resume [RUN_PREFIX]
    ");
    assert!(output.status.success(), "rewind should succeed");

    let state = run_state(&setup.run.run_dir);
    assert!(matches!(
        state.status,
        Some(fabro_types::RunStatus::Archived { .. })
    ));
    let new_run_id = state
        .superseded_by
        .expect("rewind should record replacement run");
    let replacement = run_state_by_id(&context, &new_run_id.to_string());
    assert_eq!(
        replacement
            .checkpoint
            .and_then(|checkpoint| checkpoint.git_commit_sha),
        Some(setup.step_one_sha)
    );
}

#[test]
fn rewind_archives_source_and_records_superseded_by() {
    let context = test_context!();
    let setup = setup_seeded_git_backed_changed_run(&context);
    let before_events = run_events(&setup.run.run_dir);
    assert!(
        before_events
            .iter()
            .any(|event| event.event.event_name() == "run.completed"),
        "setup run should be completed before rewind"
    );

    let mut cmd = context.command();
    cmd.args(["rewind", &setup.run.run_id, "@2"]);
    let output = cmd.output().expect("rewind should execute");
    assert!(
        output.status.success(),
        "rewind should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        support_stderr(&output),
    );

    let after_events = run_events(&setup.run.run_dir);
    assert_eq!(
        after_events.len(),
        before_events.len() + 2,
        "rewind should append run.archived and run.superseded_by"
    );
    assert_eq!(
        after_events[..before_events.len()]
            .iter()
            .map(|event| event.event.event_name())
            .collect::<Vec<_>>(),
        before_events
            .iter()
            .map(|event| event.event.event_name())
            .collect::<Vec<_>>(),
        "rewind should preserve the prior event prefix"
    );
    assert_eq!(
        after_events[before_events.len()].event.event_name(),
        "run.archived"
    );
    assert_eq!(
        after_events[before_events.len() + 1].event.event_name(),
        "run.superseded_by"
    );

    let state = run_state(&setup.run.run_dir);
    assert!(matches!(
        state.status,
        Some(fabro_types::RunStatus::Archived { .. })
    ));
    assert!(state.superseded_by.is_some());
}
