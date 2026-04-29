use fabro_test::{fabro_snapshot, run_and_format, test_context};
use insta::assert_snapshot;

use super::support::{
    git_filters, output_stdout, run_state_by_id, setup_seeded_git_backed_changed_run,
};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["fork", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Fork a workflow run from an earlier checkpoint into a new run

    Usage: fabro fork [OPTIONS] <RUN_ID> [TARGET]

    Arguments:
      <RUN_ID>  Run ID (or unambiguous prefix)
      [TARGET]  Target checkpoint: node name, node@visit, or @ordinal (omit to fork from latest)

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --list              Show the checkpoint timeline instead of forking
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn fork_outside_git_repo_errors() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["fork", "01ARZ3NDEKTSV4RRFFQ69G5FAW"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × No run found matching '[ULID]' (tried run ID prefix and workflow name)
    ");
}

#[test]
fn fork_latest_prints_new_run_and_resume_hint() {
    let context = test_context!();
    let setup = setup_seeded_git_backed_changed_run(&context);

    let mut cmd = context.command();
    cmd.args(["fork", &setup.run.run_id]);

    let (snapshot, output) = run_and_format(&mut cmd, &git_filters(&context));
    assert_snapshot!(snapshot, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----

    Forked run [RUN_PREFIX] -> [RUN_PREFIX]
    To resume: fabro resume [RUN_PREFIX]
    ");
    assert!(output.status.success(), "fork should succeed");
}

#[test]
fn fork_from_earlier_checkpoint_uses_expected_sha() {
    let context = test_context!();
    let setup = setup_seeded_git_backed_changed_run(&context);

    let output = context
        .command()
        .args(["fork", &setup.run.run_id, "@2", "--json"])
        .output()
        .expect("fork should execute");
    assert!(
        output.status.success(),
        "fork should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let fork_response: serde_json::Value =
        serde_json::from_str(&output_stdout(&output)).expect("fork json should parse");
    let new_run_id = fork_response["new_run_id"]
        .as_str()
        .expect("fork json should include new_run_id");
    let run_snapshot = run_state_by_id(&context, new_run_id);
    assert_eq!(
        run_snapshot
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.current_node.as_str()),
        Some("step_one")
    );
    assert_eq!(
        run_snapshot
            .checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.git_commit_sha.as_deref()),
        Some(setup.step_one_sha.as_str())
    );

    assert_eq!(
        run_snapshot
            .spec
            .as_ref()
            .and_then(|spec| spec.fork_source_ref.as_ref())
            .map(|source| source.checkpoint_sha.as_str()),
        Some(setup.step_one_sha.as_str())
    );
    assert_eq!(
        run_snapshot
            .spec
            .as_ref()
            .and_then(|spec| spec.fork_source_ref.as_ref())
            .map(|source| source.source_run_id.to_string()),
        Some(setup.run.run_id.clone())
    );
}
