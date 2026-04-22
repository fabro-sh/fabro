#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::fs;

use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["uninstall", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Uninstall Fabro from this machine

    Usage: fabro uninstall [OPTIONS]

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --yes               Skip confirmation prompt
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

fn command_with_no_fabro_home(context: &fabro_test::TestContext) -> assert_cmd::Command {
    let mut cmd = context.command();
    cmd.env(
        "FABRO_HOME",
        context.temp_dir.join("nonexistent-fabro-home"),
    );
    cmd
}

#[test]
fn not_installed_prints_message() {
    let context = test_context!();
    let mut cmd = command_with_no_fabro_home(&context);
    cmd.arg("uninstall");
    let output = cmd.output().expect("command should run");

    let stderr = String::from_utf8(output.stderr).unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "expected exit 0.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("Fabro is not installed."),
        "expected 'Fabro is not installed.' in stderr, got: {stderr}"
    );
}

#[test]
fn dry_run_shows_preview_without_deleting() {
    let context = test_context!();
    let fabro_home = context.home_dir.join(".fabro");
    fs::create_dir_all(fabro_home.join("certs")).unwrap();
    fs::write(fabro_home.join("settings.toml"), "# fabro settings\n").unwrap();

    let mut cmd = context.command();
    cmd.arg("uninstall");
    let output = cmd.output().expect("command should run");

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Pass --yes to confirm."),
        "expected dry-run hint in stderr, got: {stderr}"
    );
    assert!(fabro_home.exists(), "dry run should not delete ~/.fabro");
}

#[test]
fn yes_removes_fabro_home() {
    let context = test_context!();
    let fabro_home = context.home_dir.join(".fabro");
    fs::create_dir_all(fabro_home.join("certs")).unwrap();
    fs::write(fabro_home.join("settings.toml"), "# fabro settings\n").unwrap();

    let mut cmd = context.command();
    cmd.args(["uninstall", "--yes"]);
    let output = cmd.output().expect("command should run");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !fabro_home.exists(),
        "~/.fabro should be removed after --yes"
    );
}

#[test]
fn dry_run_json_outputs_inventory() {
    let context = test_context!();
    let fabro_home = context.home_dir.join(".fabro");
    fs::create_dir_all(fabro_home.join("certs")).unwrap();
    fs::write(fabro_home.join("settings.toml"), "# fabro settings\n").unwrap();

    let output = context
        .command()
        .args(["--json", "uninstall"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("uninstall --json should parse");

    assert!(value["home_exists"].as_bool().unwrap_or(false));
    assert!(value["home_root"].as_str().is_some());
    assert!(value["home_size"].is_number());
    assert!(value.get("server_running").is_some());
    assert!(value.get("shell_configs").is_some());
}

#[test]
fn yes_json_outputs_result() {
    let context = test_context!();
    let fabro_home = context.home_dir.join(".fabro");
    fs::create_dir_all(fabro_home.join("certs")).unwrap();
    fs::write(fabro_home.join("settings.toml"), "# fabro settings\n").unwrap();

    let output = context
        .command()
        .args(["--json", "uninstall", "--yes"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("uninstall --yes --json should parse");

    assert_eq!(value["status"].as_str(), Some("completed"));
    assert_eq!(value["home_removed"].as_bool(), Some(true));
    assert!(
        !fabro_home.exists(),
        "~/.fabro should be removed after --yes --json"
    );
}

#[test]
fn not_installed_json() {
    let context = test_context!();
    let mut cmd = command_with_no_fabro_home(&context);
    cmd.args(["--json", "uninstall"]);
    let output = cmd.output().expect("command should run");

    let stderr = String::from_utf8(output.stderr).unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "expected exit 0.\nstdout: {stdout}\nstderr: {stderr}"
    );
    let value: Value = serde_json::from_str(&stdout).expect("uninstall --json should parse");

    assert_eq!(value["status"].as_str(), Some("not_installed"));
}
