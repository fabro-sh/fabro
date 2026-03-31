use insta::assert_snapshot;

use fabro_test::{fabro_snapshot, run_and_format, test_context};

use super::support::{
    git_filters, git_show_json, git_stdout, metadata_run_ids, run_branch_commits,
    run_branch_commits_since_base, setup_git_backed_changed_run,
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
          --json                       Output as JSON [env: FABRO_JSON=]
          --list                       Show the checkpoint timeline instead of forking
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-push                    Skip pushing new branches to the remote
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn fork_outside_git_repo_errors() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.current_dir(&context.temp_dir);
    cmd.args(["fork", "01ARZ3NDEKTSV4RRFFQ69G5FAW"]);

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
fn fork_latest_prints_new_run_and_resume_hint() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let before = metadata_run_ids(&setup.repo_dir);

    let mut cmd = context.command();
    cmd.current_dir(&setup.repo_dir);
    cmd.args(["fork", &setup.run.run_id, "--no-push"]);

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

    let after = metadata_run_ids(&setup.repo_dir);
    let new_run_ids: Vec<_> = after.difference(&before).cloned().collect();
    assert_eq!(
        new_run_ids.len(),
        1,
        "fork should create one new run branch"
    );
    let new_run_id = &new_run_ids[0];

    let new_head = git_stdout(
        &setup.repo_dir,
        &["rev-parse", &format!("fabro/run/{new_run_id}")],
    );
    let expected_head = run_branch_commits(&setup.repo_dir, &setup.run.run_id)
        .into_iter()
        .last()
        .expect("source run should have a last run commit");
    assert_eq!(new_head.trim(), expected_head);
}

#[test]
fn fork_from_earlier_checkpoint_uses_expected_sha() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);
    let before = metadata_run_ids(&setup.repo_dir);
    let expected_head =
        run_branch_commits_since_base(&setup.repo_dir, &setup.run.run_id, &setup.base_sha)
            .into_iter()
            .next()
            .expect("source run should have a first run commit");

    let output = context
        .command()
        .current_dir(&setup.repo_dir)
        .args(["fork", &setup.run.run_id, "@1", "--no-push"])
        .output()
        .expect("fork should execute");
    assert!(
        output.status.success(),
        "fork should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let after = metadata_run_ids(&setup.repo_dir);
    let new_run_ids: Vec<_> = after.difference(&before).cloned().collect();
    assert_eq!(
        new_run_ids.len(),
        1,
        "fork should create one new run branch"
    );
    let new_run_id = &new_run_ids[0];

    let new_head = git_stdout(
        &setup.repo_dir,
        &["rev-parse", &format!("fabro/run/{new_run_id}")],
    );
    assert_eq!(new_head.trim(), expected_head);

    let checkpoint = git_show_json(
        &setup.repo_dir,
        &format!("fabro/meta/{new_run_id}:checkpoint.json"),
    );
    assert_eq!(checkpoint["current_node"].as_str(), Some("step_one"));
    assert_eq!(
        checkpoint["git_commit_sha"].as_str(),
        Some(expected_head.as_str())
    );

    let start = git_show_json(
        &setup.repo_dir,
        &format!("fabro/meta/{new_run_id}:start.json"),
    );
    let expected_branch = format!("fabro/run/{new_run_id}");
    assert_eq!(start["run_branch"].as_str(), Some(expected_branch.as_str()));
}
