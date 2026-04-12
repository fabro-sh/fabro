use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_local_sandbox_run;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["sandbox", "ssh", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    SSH into a run's sandbox

    Usage: fabro sandbox ssh [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID or prefix

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --ttl <TTL>         SSH access expiry in minutes (default 60) [default: 60]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --print             Print the SSH command instead of connecting
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn sandbox_ssh_rejects_non_daytona_run() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);
    let mut cmd = context.ssh();
    cmd.args([&setup.run.run_id, "--print"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Sandbox provider does not support this capability.
    ");
}

#[test]
fn sandbox_ssh_json_without_print_is_rejected() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "sandbox", "ssh", "ignored-run"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json is not supported for this command"));
}
