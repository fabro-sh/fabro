use fabro_test::{fabro_snapshot, test_context};

fn isolated_storage_dir() -> tempfile::TempDir {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::create_dir_all(root.path().join("storage")).unwrap();
    root
}

fn wait_for_path(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timed out waiting for {}", path.display());
}

fn stop_pid(pid: u32) {
    fabro_proc::sigterm(pid);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !fabro_proc::process_alive(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    fabro_proc::sigkill(pid);
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["server", "status", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Show server status

    Usage: fabro server status [OPTIONS]

    Options:
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --json                       Output as JSON
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn status_when_not_running() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "status"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Server is not running
    ");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This integration test moves a live daemon record on disk to simulate an unsupported legacy daemon upgrade."
)]
fn status_errors_when_only_a_legacy_running_server_record_exists() {
    let home_dir = tempfile::tempdir_in("/tmp").unwrap();
    let fabro_home = home_dir.path().join(".fabro");
    let storage_dir = fabro_home.join("storage");
    let socket_path = home_dir.path().join("legacy.sock");
    let config_dir = tempfile::tempdir_in("/tmp").unwrap();
    let config_path = config_dir.path().join("settings.toml");
    std::fs::write(&config_path, "_version = 1\n").unwrap();

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

    let status_output = {
        let mut status = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
        fabro_test::apply_test_isolation(&mut status, home_dir.path());
        status
            .args(["server", "status"])
            .output()
            .expect("server status should run")
    };

    let pid = serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string(&legacy_record).unwrap(),
    )
    .unwrap()["pid"]
        .as_u64()
        .unwrap() as u32;
    stop_pid(pid);
    let _ = std::fs::remove_file(&legacy_record);
    let _ = std::fs::remove_file(&socket_path);

    assert!(
        !status_output.status.success(),
        "server status should fail when only the legacy record exists"
    );
    let stderr = String::from_utf8_lossy(&status_output.stderr);
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
}
