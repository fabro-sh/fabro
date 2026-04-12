use fabro_test::{fabro_snapshot, test_context};

use super::support::{git_stdout, output_stderr, setup_git_backed_changed_run};

const SHARED_DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
      -d, --detach            Run in the background and print the run ID
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
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

    Usage: fabro resume --no-upgrade-check <RUN>

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
    let rewound_head = git_stdout(&setup.repo_dir, &[
        "rev-parse",
        &format!("fabro/run/{}", setup.run.run_id),
    ]);

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
    let resumed_head = git_stdout(&setup.repo_dir, &[
        "rev-parse",
        &format!("fabro/run/{}", setup.run.run_id),
    ]);
    assert_ne!(resumed_head.trim(), rewound_head.trim());
}

#[test]
fn resume_detached_does_not_create_launcher_record() {
    let context = test_context!();
    let setup = setup_git_backed_changed_run(&context);

    context
        .command()
        .current_dir(&setup.repo_dir)
        .args(["rewind", &setup.run.run_id, "@1", "--no-push"])
        .assert()
        .success();

    let mut resume_cmd = context.command();
    resume_cmd.current_dir(&setup.repo_dir);
    resume_cmd.env("OPENAI_API_KEY", "test");
    resume_cmd.args(["resume", "--detach", &setup.run.run_id]);
    let resume_output = resume_cmd.output().expect("resume should execute");
    assert!(
        resume_output.status.success(),
        "resume should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&resume_output.stdout),
        output_stderr(&resume_output)
    );

    assert!(
        !context
            .storage_dir
            .join("launchers")
            .join(format!("{}.json", setup.run.run_id))
            .exists(),
        "server-owned resume should not create a launcher record"
    );

    context
        .command()
        .args(["wait", &setup.run.run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();
}
