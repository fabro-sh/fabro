#![allow(clippy::absolute_paths)]

use std::process::Output;

use fabro_test::{fabro_snapshot, test_context, twin_openai};
use predicates::prelude::*;

async fn run_success_output(mut cmd: assert_cmd::Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .expect("blocking command task should complete")
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.doctor();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Check environment and integration health

    Usage: fabro doctor [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
      -v, --verbose                    Show detailed information for each check
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn dry_run_flag_is_rejected() {
    let context = test_context!();
    let mut cmd = context.doctor();
    cmd.arg("--dry-run");
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: unexpected argument '--dry-run' found

    Usage: fabro doctor [OPTIONS]

    For more information, try '--help'.
    ");
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn live_doctor() {
    let context = test_context!();
    context.doctor().assert().success();
}

#[fabro_macros::e2e_test(twin)]
async fn twin_doctor() {
    let context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    let mut cmd = context.doctor();
    cmd.arg("--verbose");
    cmd.env_clear();
    cmd.env("NO_COLOR", "1");
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true");
    cmd.env("FABRO_STORAGE_DIR", &context.storage_dir);
    cmd.env(
        "PATH",
        "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    );
    twin.configure_command(&mut cmd, &namespace);

    let output = run_success_output(cmd).await;
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.to_lowercase().contains("openai connectivity: ok"),
        "expected verbose doctor output to include openai probe success, got: {stdout}"
    );
}

#[test]
fn doctor_no_color_when_no_color_set() {
    let context = test_context!();
    let mut cmd = context.doctor();
    cmd.env_clear();
    cmd.env("NO_COLOR", "1");
    cmd.assert().stdout(predicate::str::contains("\x1b[").not());
}
