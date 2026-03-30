use fabro_test::{fabro_snapshot, test_context};

use super::support::{output_stdout, write_sleep_workflow};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["attach", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Attach to a running or finished workflow run

    Usage: fabro attach [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name

    Options:
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
fn attach_before_completion_streams_to_finished_state() {
    let context = test_context!();
    write_sleep_workflow(
        &context.temp_dir.join("slow.fabro"),
        "slow",
        "Run slowly",
        2,
    );

    let mut run_cmd = context.command();
    run_cmd.current_dir(&context.temp_dir);
    run_cmd.env("OPENAI_API_KEY", "test");
    run_cmd.args([
        "run",
        "--detach",
        "--provider",
        "openai",
        "--sandbox",
        "local",
        "--no-retro",
        "slow.fabro",
    ]);
    let run_output = run_cmd.output().expect("command should execute");
    assert!(
        run_output.status.success(),
        "run --detach failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let run_id = output_stdout(&run_output).trim().to_string();

    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut attach_cmd = context.command();
    attach_cmd.current_dir(&context.temp_dir);
    attach_cmd.args(["attach", &run_id]);

    fabro_snapshot!(filters, attach_cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
        Sandbox: local (ready in [TIME])
        ✓ start  [DURATION]
        ✓ wait  [DURATION]
        ✓ exit  [DURATION]
    ");
}
