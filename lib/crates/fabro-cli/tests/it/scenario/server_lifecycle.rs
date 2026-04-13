use fabro_test::{fabro_snapshot, test_context};

#[test]
fn start_status_stop_lifecycle() {
    let context = test_context!();
    let storage_root = tempfile::tempdir_in("/tmp").unwrap();
    let storage_dir = storage_root.path().join("storage");
    std::fs::create_dir_all(&storage_dir).unwrap();

    let sock_dir = tempfile::tempdir_in("/tmp").unwrap();
    let bind_addr = sock_dir.path().join("test.sock");
    let bind_str = bind_addr.to_string_lossy().to_string();

    let mut filters = context.filters();
    filters.push((r"pid \d+".to_string(), "pid [PID]".to_string()));
    filters.push((regex::escape(&bind_str), "[SOCKET_PATH]".to_string()));
    filters.push((
        r"started \d+[hms] (?:\d+[hms] )*ago".to_string(),
        "started [UPTIME] ago".to_string(),
    ));
    filters.push((
        r"fabro_dev_[0-9a-f]{64}".to_string(),
        "fabro_dev_[DEV_TOKEN]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "start", "--dry-run", "--bind", &bind_str]);
    fabro_snapshot!(filters.clone(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Server started (pid [PID]) on [SOCKET_PATH]
    Dev token: fabro_dev_[DEV_TOKEN]
    Token file: [HOME_DIR]/.fabro/dev-token
    ");

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "status"]);
    fabro_snapshot!(filters.clone(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Server running (pid [PID]) on [SOCKET_PATH], started [UPTIME] ago
    ");

    let status_output = context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "status", "--json"])
        .assert()
        .success();
    let stdout = std::str::from_utf8(&status_output.get_output().stdout)
        .expect("status --json stdout should be valid UTF-8");
    let json: serde_json::Value =
        serde_json::from_str(stdout).expect("status --json should be valid JSON");
    assert_eq!(
        json["status"].as_str(),
        Some("running"),
        "status should be running"
    );

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "stop"]);
    fabro_snapshot!(filters.clone(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Server stopped
    ");

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "status"]);
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Server is not running
    ");

    assert!(
        !storage_dir.join("server.json").exists(),
        "server.json should be removed after stop"
    );
}
