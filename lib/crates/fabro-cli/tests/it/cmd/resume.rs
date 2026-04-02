use fabro_test::{fabro_snapshot, test_context};

use super::support::{git_stdout, output_stderr, setup_git_backed_changed_run};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["resume", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Resume an interrupted workflow run

    Usage: fabro resume [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID or unambiguous prefix

    Options:
      -d, --detach                     Run in the background and print the run ID
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn resume_requires_run_arg() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["resume"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUN>

    Usage: fabro resume --no-upgrade-check --storage-dir <STORAGE_DIR> <RUN>

    For more information, try '--help'.
    ");
}

#[test]
fn resume_rewound_run_succeeds() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);

    let rewind = context
        .command()
        .current_dir(&setup.repo_dir)
        .args(["rewind", &setup.run.run_id, "@1", "--no-push"])
        .output()
        .expect("rewind should execute");
    assert!(
        rewind.status.success(),
        "rewind should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&rewind.stdout),
        output_stderr(&rewind)
    );
    let rewound_head = git_stdout(
        &setup.repo_dir,
        &["rev-parse", &format!("fabro/run/{}", setup.run.run_id)],
    );
    std::fs::remove_file(setup.run.run_dir.join("run.json")).unwrap();

    let mut resume_cmd = context.command();
    resume_cmd.current_dir(&setup.repo_dir);
    resume_cmd.env("OPENAI_API_KEY", "test");
    resume_cmd.args(["resume", &setup.run.run_id]);
    let resume_output = resume_cmd.output().expect("resume should execute");
    assert!(
        resume_output.status.success(),
        "resume should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&resume_output.stdout),
        output_stderr(&resume_output)
    );

    let mut wait_filters = context.filters();
    wait_filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut wait_cmd = context.command();
    wait_cmd.args(["wait", &setup.run.run_id]);
    fabro_snapshot!(wait_filters, wait_cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Succeeded [ULID]  [DURATION]
    ");

    assert_eq!(
        std::fs::read_to_string(setup.run.run_dir.join("worktree/story.txt")).unwrap(),
        "line 1\nline 2\nline 3\n"
    );
    assert_eq!(
        std::fs::read_to_string(setup.repo_dir.join("story.txt")).unwrap(),
        "line 1\n"
    );
    let resumed_head = git_stdout(
        &setup.repo_dir,
        &["rev-parse", &format!("fabro/run/{}", setup.run.run_id)],
    );
    assert_ne!(resumed_head.trim(), rewound_head.trim());
}
