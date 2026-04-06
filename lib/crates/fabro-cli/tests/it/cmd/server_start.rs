use fabro_test::{fabro_snapshot, test_context};
use std::process::Stdio;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

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
              Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --debug
              Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --foreground
              Run in the foreground instead of daemonizing
          --bind <BIND>
              Address to bind to (host:port for TCP, or path containing / for Unix socket)
          --no-upgrade-check
              Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --model <MODEL>
              Override default LLM model
          --quiet
              Suppress non-essential output [env: FABRO_QUIET=]
          --provider <PROVIDER>
              Override default LLM provider
          --verbose
              Enable verbose output [env: FABRO_VERBOSE=]
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
fn concurrent_autostart_converges_on_one_shared_daemon_and_cleans_up() {
    fn run_ps_json(
        home_dir: &std::path::Path,
        temp_dir: &std::path::Path,
        storage_dir: &std::path::Path,
    ) -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_fabro"))
            .current_dir(temp_dir)
            .env("NO_COLOR", "1")
            .env("HOME", home_dir)
            .env("FABRO_NO_UPGRADE_CHECK", "true")
            .env("FABRO_STORAGE_DIR", storage_dir)
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
            .filter(|line| line.contains("fabro: server") && line.contains(socket_path))
            .count()
    }

    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let socket_path = storage_dir.join("fabro.sock").display().to_string();
    let home_a = tempfile::tempdir_in("/tmp").unwrap();
    let home_b = tempfile::tempdir_in("/tmp").unwrap();
    let temp_a = tempfile::tempdir_in("/tmp").unwrap();
    let temp_b = tempfile::tempdir_in("/tmp").unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let barrier_a = Arc::clone(&barrier);
    let storage_a = storage_dir.clone();
    let thread_a = std::thread::spawn(move || {
        barrier_a.wait();
        run_ps_json(home_a.path(), temp_a.path(), &storage_a)
    });

    let barrier_b = Arc::clone(&barrier);
    let storage_b = storage_dir.clone();
    let thread_b = std::thread::spawn(move || {
        barrier_b.wait();
        run_ps_json(home_b.path(), temp_b.path(), &storage_b)
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
        if storage_dir.join("server.json").exists() && daemon_match_count(&socket_path) == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        storage_dir.join("server.json").exists(),
        "shared storage should have an active server record"
    );
    assert_eq!(
        daemon_match_count(&socket_path),
        1,
        "concurrent auto-start should converge on one daemon"
    );

    let stop = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"))
        .env("NO_COLOR", "1")
        .env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "stop"])
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
        if !storage_dir.join("server.json").exists() && daemon_match_count(&socket_path) == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !storage_dir.join("server.json").exists(),
        "last TestContext drop should remove the server record"
    );
    assert_eq!(
        daemon_match_count(&socket_path),
        0,
        "last TestContext drop should clean up the shared daemon"
    );
}
