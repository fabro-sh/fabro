#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::fs;

use fabro_test::{fabro_snapshot, stop_pid, test_context, wait_for_path};
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

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This integration test moves a live daemon record on disk to simulate an unsupported legacy daemon upgrade."
)]
fn uninstall_yes_fails_when_only_a_legacy_running_server_record_exists() {
    let home_dir = tempfile::tempdir_in("/tmp").unwrap();
    let fabro_home = home_dir.path().join(".fabro");
    let storage_dir = fabro_home.join("storage");
    let socket_path = home_dir.path().join("legacy.sock");
    let config_dir = tempfile::tempdir_in("/tmp").unwrap();
    let config_path = config_dir.path().join("settings.toml");
    std::fs::write(&config_path, "_version = 1\n").unwrap();
    std::fs::create_dir_all(&fabro_home).unwrap();
    std::fs::write(fabro_home.join("settings.toml"), "_version = 1\n").unwrap();

    let start_output = {
        let mut start = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
        fabro_test::apply_test_isolation(&mut start, home_dir.path());
        start
            .args(["server", "start", "--bind"])
            .arg(&socket_path)
            .arg("--config")
            .arg(&config_path)
            .output()
            .expect("server start should run")
    };
    assert!(
        start_output.status.success(),
        "server start should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&start_output.stdout),
        String::from_utf8_lossy(&start_output.stderr)
    );

    let current_record = storage_dir.join("server.json");
    wait_for_path(&current_record);
    let legacy_record = fabro_home.join("server.json");
    std::fs::rename(&current_record, &legacy_record).unwrap();
    let pid_u64 = serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string(&legacy_record).unwrap(),
    )
    .unwrap()["pid"]
        .as_u64()
        .unwrap();
    let pid = u32::try_from(pid_u64).expect("pid fits in u32");

    let uninstall_output = {
        let mut uninstall = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
        fabro_test::apply_test_isolation(&mut uninstall, home_dir.path());
        uninstall
            .args(["uninstall", "--yes"])
            .output()
            .expect("uninstall should run")
    };

    stop_pid(pid);
    let _ = std::fs::remove_file(&legacy_record);
    let _ = std::fs::remove_file(&socket_path);

    assert!(
        !uninstall_output.status.success(),
        "uninstall --yes should fail when only the legacy record exists"
    );
    let stderr = String::from_utf8_lossy(&uninstall_output.stderr);
    assert!(
        stderr.contains(&legacy_record.display().to_string()),
        "expected stderr to mention the legacy record path, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&current_record.display().to_string()),
        "expected stderr to mention the current record path, got:\n{stderr}"
    );
    assert!(
        stderr.contains("legacy Fabro CLI"),
        "expected stderr to instruct manual cleanup, got:\n{stderr}"
    );
    assert!(
        fabro_home.exists(),
        "uninstall should not remove ~/.fabro when the legacy daemon detector fires"
    );
}
