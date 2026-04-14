use std::process::Stdio;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use fabro_test::{fabro_snapshot, test_context};

fn isolated_storage_dir() -> tempfile::TempDir {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::create_dir_all(root.path().join("storage")).unwrap();
    root
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["server", "start", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Start the HTTP API server

    Usage: fabro server start [OPTIONS]

    Options:
          --json
              Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>
              Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug
              Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --foreground
              Run in the foreground instead of daemonizing
          --bind <BIND>
              Address to bind to (IP or IP:port for TCP, or path containing / for Unix socket)
          --no-upgrade-check
              Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet
              Suppress non-essential output [env: FABRO_QUIET=]
          --web
              Enable the embedded web UI and browser auth routes
          --no-web
              Disable the embedded web UI, browser auth routes, and web-only helper endpoints
          --verbose
              Enable verbose output [env: FABRO_VERBOSE=]
          --model <MODEL>
              Override default LLM model
          --provider <PROVIDER>
              Override default LLM provider
          --dry-run
              Execute with simulated LLM backend
          --sandbox <SANDBOX>
              Sandbox for agent tools
          --max-concurrent-runs <MAX_CONCURRENT_RUNS>
              Maximum number of concurrent run executions
          --config <CONFIG>
              Path to server config file (default: ~/.fabro/settings.toml)
      -h, --help
              Print help
    ----- stderr -----
    ");
}

#[test]
fn start_already_running_exits_with_error() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");

    let sock_dir = tempfile::tempdir_in("/tmp").unwrap();
    let bind_addr = sock_dir.path().join("test.sock");
    let bind_str = bind_addr.to_string_lossy().to_string();

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "start", "--dry-run", "--bind", &bind_str])
        .assert()
        .success();

    let mut filters = context.filters();
    filters.push((r"pid \d+".to_string(), "pid [PID]".to_string()));
    filters.push((regex::escape(&bind_str), "[SOCKET_PATH]".to_string()));
    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "start", "--dry-run", "--bind", &bind_str]);
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Server already running (pid [PID]) on [SOCKET_PATH]
    ");

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "stop"])
        .assert()
        .success();
}

#[test]
fn start_without_bind_uses_home_socket_instead_of_storage_socket() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let expected_socket = context.home_dir.join(".fabro").join("fabro.sock");
    let storage_socket = storage_dir.join("fabro.sock");

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "start", "--dry-run"])
        .assert()
        .success();

    let output = context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(json["bind"].as_str(), expected_socket.to_str());
    assert_ne!(json["bind"].as_str(), storage_socket.to_str());

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "stop"])
        .assert()
        .success();
}

#[test]
fn start_with_tcp_host_only_bind_resolves_to_host_and_port() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "start", "--dry-run", "--bind", "127.0.0.1"]);
    let output = cmd.output().expect("server start command should run");
    assert!(
        output.status.success(),
        "server start should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Server started (pid "),
        "expected startup message, got {stderr}"
    );
    let bind_regex = regex::Regex::new(r"127\.0\.0\.1:\d+").unwrap();
    assert!(
        bind_regex.is_match(&stderr),
        "expected resolved tcp bind in stderr, got {stderr}"
    );

    let output = context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let bind = json["bind"].as_str().expect("bind should be a string");
    assert!(
        bind.starts_with("127.0.0.1:"),
        "expected resolved tcp bind, got {bind}"
    );

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "stop"])
        .assert()
        .success();
}

#[test]
fn start_with_tcp_host_only_bind_warns_and_falls_back_when_default_port_is_unavailable() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let occupied = match std::net::TcpListener::bind(("127.0.0.1", 32276)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            return;
        }
        Err(error) => panic!("failed to bind default TCP port 32276: {error}"),
    };

    let mut filters = context.filters();
    filters.push((r"pid \d+".to_string(), "pid [PID]".to_string()));
    filters.push((r"127\.0\.0\.1:\d+".to_string(), "[TCP_BIND]".to_string()));
    filters.push((
        r"fabro_dev_[0-9a-f]{64}".to_string(),
        "fabro_dev_[DEV_TOKEN]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "start", "--dry-run", "--bind", "127.0.0.1"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Warning: TCP port 32276 is unavailable on 127.0.0.1; falling back to a random port.
    Server started (pid [PID]) on [TCP_BIND]
    Dev token: fabro_dev_[DEV_TOKEN]
    Token file: [HOME_DIR]/.fabro/dev-token
    ");

    let output = context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let bind = json["bind"].as_str().expect("bind should be a string");
    assert_ne!(bind, "127.0.0.1:32276");
    assert!(
        bind.starts_with("127.0.0.1:"),
        "expected resolved tcp bind, got {bind}"
    );

    drop(occupied);

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "stop"])
        .assert()
        .success();
}

#[test]
fn default_test_contexts_share_one_eager_session_server() {
    let context_a = test_context!();
    let context_b = test_context!();

    assert_eq!(
        context_a.storage_dir, context_b.storage_dir,
        "default test contexts in one session should share storage owned by one server"
    );

    let output_a = context_a
        .command()
        .args(["server", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output_b = context_b
        .command()
        .args(["server", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let status_a: serde_json::Value = serde_json::from_slice(&output_a).unwrap();
    let status_b: serde_json::Value = serde_json::from_slice(&output_b).unwrap();

    assert_eq!(status_a["status"].as_str(), Some("running"));
    assert_eq!(status_a["pid"], status_b["pid"]);
}

#[test]
fn default_test_context_server_keeps_object_store_off_disk() {
    let context = test_context!();

    context
        .command()
        .args(["server", "status", "--json"])
        .assert()
        .success();

    assert!(
        !context.storage_dir.join("store").exists(),
        "shared test daemon should not materialize on-disk object store files"
    );
}

#[test]
fn isolated_server_switches_context_to_separate_daemon() {
    let mut context = test_context!();
    let shared_storage_dir = context.storage_dir.clone();
    let shared_status = context
        .command()
        .args(["server", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let shared_status: serde_json::Value = serde_json::from_slice(&shared_status).unwrap();

    context.isolated_server();

    assert_ne!(
        context.storage_dir, shared_storage_dir,
        "isolated_server should move the context onto a separate server-owned storage dir"
    );

    let isolated_status = context
        .command()
        .args(["server", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let isolated_status: serde_json::Value = serde_json::from_slice(&isolated_status).unwrap();

    assert_eq!(isolated_status["status"].as_str(), Some("running"));
    assert_ne!(isolated_status["pid"], shared_status["pid"]);
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration test uses OS threads to exercise concurrent CLI auto-start behavior across separate processes."
)]
fn concurrent_autostart_converges_on_one_shared_daemon_and_cleans_up() {
    fn run_ps_json(
        home_dir: &std::path::Path,
        temp_dir: &std::path::Path,
        config_path: &std::path::Path,
    ) -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_fabro"))
            .current_dir(temp_dir)
            .env("FABRO_TEST_IN_MEMORY_STORE", "1")
            .env("NO_COLOR", "1")
            .env("HOME", home_dir)
            .env("FABRO_HOME", home_dir.join(".fabro"))
            .env("FABRO_CONFIG", config_path)
            .env("FABRO_NO_UPGRADE_CHECK", "true")
            .env("FABRO_HTTP_PROXY_POLICY", "disabled")
            .args(["ps", "-a", "--json"])
            .output()
            .expect("ps command should execute")
    }

    fn daemon_match_count(socket_path: &str) -> usize {
        let output = std::process::Command::new("ps")
            .args(["-ww", "-axo", "command="])
            .stdout(Stdio::piped())
            .output()
            .expect("ps should execute");
        assert!(output.status.success(), "ps should succeed");
        String::from_utf8(output.stdout)
            .expect("ps output should be UTF-8")
            .lines()
            .filter(|line| line.contains("fabro server") && line.contains(socket_path))
            .count()
    }

    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let socket_path = storage_root.path().join("shared.sock");
    let socket_path_str = socket_path.display().to_string();
    let config_dir = tempfile::tempdir_in("/tmp").unwrap();
    let config_path = config_dir.path().join("settings.toml");
    std::fs::write(
        &config_path,
        format!(
            "_version = 1\n\n[server.storage]\nroot = \"{}\"\n\n[cli.target]\ntype = \"unix\"\npath = \"{}\"\n",
            storage_dir.display(),
            socket_path.display()
        ),
    )
    .unwrap();
    let home_a = tempfile::tempdir_in("/tmp").unwrap();
    let home_b = tempfile::tempdir_in("/tmp").unwrap();
    let temp_a = tempfile::tempdir_in("/tmp").unwrap();
    let temp_b = tempfile::tempdir_in("/tmp").unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let barrier_a = Arc::clone(&barrier);
    let config_a = config_path.clone();
    let thread_a = std::thread::spawn(move || {
        barrier_a.wait();
        run_ps_json(home_a.path(), temp_a.path(), &config_a)
    });

    let barrier_b = Arc::clone(&barrier);
    let config_b = config_path.clone();
    let thread_b = std::thread::spawn(move || {
        barrier_b.wait();
        run_ps_json(home_b.path(), temp_b.path(), &config_b)
    });

    barrier.wait();
    let output_a = thread_a.join().expect("thread A should join");
    let output_b = thread_b.join().expect("thread B should join");
    assert!(
        output_a.status.success(),
        "first concurrent ps should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output_a.stdout),
        String::from_utf8_lossy(&output_a.stderr)
    );
    assert!(
        output_b.status.success(),
        "second concurrent ps should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output_b.stdout),
        String::from_utf8_lossy(&output_b.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if storage_dir.join("server.json").exists() && daemon_match_count(&socket_path_str) == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        storage_dir.join("server.json").exists(),
        "shared storage should have an active server record"
    );
    assert_eq!(
        daemon_match_count(&socket_path_str),
        1,
        "concurrent auto-start should converge on one daemon"
    );

    let stop = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"))
        .env("FABRO_TEST_IN_MEMORY_STORE", "1")
        .env("NO_COLOR", "1")
        .env("FABRO_CONFIG", &config_path)
        .env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled")
        .args(["server", "stop", "--timeout", "0"])
        .output()
        .expect("server stop should execute");
    assert!(
        stop.status.success(),
        "server stop should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !storage_dir.join("server.json").exists() && daemon_match_count(&socket_path_str) == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !storage_dir.join("server.json").exists(),
        "last TestContext drop should remove the server record"
    );
    assert_eq!(
        daemon_match_count(&socket_path_str),
        0,
        "last TestContext drop should clean up the shared daemon"
    );
}
