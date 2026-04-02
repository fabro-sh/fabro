use fabro_test::{fabro_snapshot, test_context};

#[test]
#[cfg(feature = "server")]
fn start_status_stop_lifecycle() {
    let context = test_context!();

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

    // Start the server as a daemon with a Unix socket in a short path
    let mut cmd = context.command();
    cmd.args(["server", "start", "--dry-run", "--bind", &bind_str]);
    fabro_snapshot!(filters.clone(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Server started (pid [PID]) on [SOCKET_PATH]
    ");

    // Verify server.json is written in storage_dir
    assert!(
        context.storage_dir.join("server.json").exists(),
        "server.json should exist after start"
    );

    // Status should report running
    let mut cmd = context.command();
    cmd.args(["server", "status"]);
    fabro_snapshot!(filters.clone(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Server running (pid [PID]) on [SOCKET_PATH], started [UPTIME] ago
    ");

    // Status --json should produce valid JSON with "running" status
    let status_output = context
        .command()
        .args(["server", "status", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(status_output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("status --json should be valid JSON");
    assert_eq!(
        json["status"].as_str(),
        Some("running"),
        "status should be running"
    );

    // Stop the server
    let mut cmd = context.command();
    cmd.args(["server", "stop"]);
    fabro_snapshot!(filters.clone(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Server stopped
    ");

    // Status should report not running
    let mut cmd = context.command();
    cmd.args(["server", "status"]);
    fabro_snapshot!(filters.clone(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Server is not running
    ");

    // Verify server.json is cleaned up
    assert!(
        !context.storage_dir.join("server.json").exists(),
        "server.json should be removed after stop"
    );
}
