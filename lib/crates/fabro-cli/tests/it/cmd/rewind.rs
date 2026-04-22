use fabro_test::{fabro_snapshot, run_and_format, test_context};
use insta::assert_snapshot;

use super::support::{
    git_filters, git_stdout, output_stderr as support_stderr, run_branch_commits_since_base,
    run_events, run_state, setup_git_backed_changed_run,
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
          --no-push           Skip force-pushing rewound refs to the remote
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
    error: not in a git repository
      > could not find repository at '.'; class=Repository (6); code=NotFound (-3)
    ");
}

#[test]
fn rewind_list_prints_timeline_for_completed_git_run() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let mut cmd = context.command();
    cmd.current_dir(&setup.repo_dir);
    cmd.args(["rewind", &setup.run.run_id, "--list"]);

    fabro_snapshot!(git_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    @   Node      Details 
     @1  step_one          
     @2  step_two
    ");
}

#[test]
fn rewind_target_updates_metadata_and_resume_hint() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let expected_run_head =
        run_branch_commits_since_base(&setup.repo_dir, &setup.run.run_id, &setup.base_sha)
            .into_iter()
            .next()
            .expect("source run should have a first run commit");

    let mut cmd = context.command();
    cmd.current_dir(&setup.repo_dir);
    cmd.args(["rewind", &setup.run.run_id, "@1", "--no-push"]);

    let (snapshot, output) = run_and_format(&mut cmd, &git_filters(&context));
    assert_snapshot!(snapshot, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Rewound metadata branch to @1 (step_one)
    Rewound run branch fabro/run/[ULID] to [SHA]

    To resume: fabro resume [RUN_PREFIX]
    ");
    assert!(output.status.success(), "rewind should succeed");

    let run_head = git_stdout(&setup.repo_dir, &[
        "rev-parse",
        &format!("fabro/run/{}", setup.run.run_id),
    ]);
    assert_eq!(run_head.trim(), expected_run_head);

    let mut list_cmd = context.command();
    list_cmd.current_dir(&setup.repo_dir);
    list_cmd.args(["rewind", &setup.run.run_id, "--list"]);
    let list_output = list_cmd.output().expect("rewind --list should execute");
    assert!(list_output.status.success(), "rewind --list should succeed");
    let list = support_stderr(&list_output);
    assert!(
        list.contains("@1"),
        "rewound timeline should keep @1: {list}"
    );
    assert!(
        !list.contains("@2"),
        "rewound timeline should drop @2: {list}"
    );
}

#[test]
fn rewind_preserves_event_history_and_clears_terminal_snapshot_state() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let before_events = run_events(&setup.run.run_dir);
    assert!(
        before_events
            .iter()
            .any(|event| event.event.event_name() == "run.completed"),
        "setup run should be completed before rewind"
    );

    let mut cmd = context.command();
    cmd.current_dir(&setup.repo_dir);
    cmd.args(["rewind", &setup.run.run_id, "@1", "--no-push"]);
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
        before_events.len() + 3,
        "rewind should append run.rewound, checkpoint.completed, and run.submitted"
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
        "run.rewound"
    );
    assert_eq!(
        after_events[before_events.len() + 1].event.event_name(),
        "checkpoint.completed"
    );
    assert_eq!(
        after_events[before_events.len() + 2].event.event_name(),
        "run.submitted"
    );
    assert!(
        after_events[before_events.len() + 2]
            .event
            .properties()
            .unwrap()["definition_blob"]
            .is_string(),
        "rewind should re-emit run.submitted with the definition_blob"
    );

    let state = run_state(&setup.run.run_dir);
    assert_eq!(state.status, Some(fabro_types::RunStatus::Submitted));
    assert!(state.conclusion.is_none(), "rewind should clear conclusion");
    assert!(
        state.final_patch.is_none(),
        "rewind should clear final patch"
    );
    assert!(
        state.pull_request.is_none(),
        "rewind should clear pull request"
    );
    assert!(
        state.is_empty(),
        "rewind should clear node state that belonged to the prior execution"
    );
}
