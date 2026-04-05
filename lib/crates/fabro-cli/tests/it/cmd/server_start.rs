use fabro_test::{fabro_snapshot, test_context};
use std::process::Stdio;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

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
          --foreground
              Run in the foreground instead of daemonizing
          --json
              Output as JSON [env: FABRO_JSON=]
          --bind <BIND>
              Address to bind to (host:port for TCP, or path containing / for Unix socket)
          --debug
              Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --model <MODEL>
              Override default LLM model
          --no-upgrade-check
              Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --provider <PROVIDER>
              Override default LLM provider
          --quiet
              Suppress non-essential output [env: FABRO_QUIET=]
          --dry-run
              Execute with simulated LLM backend
          --verbose
              Enable verbose output [env: FABRO_VERBOSE=]
          --sandbox <SANDBOX>
              Sandbox for agent tools
          --storage-dir <STORAGE_DIR>
              Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --max-concurrent-runs <MAX_CONCURRENT_RUNS>
              Maximum number of concurrent run executions
          --server-url <SERVER_URL>
              Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
          --config <CONFIG>
              Path to server config file (default: ~/.fabro/server.toml)
      -h, --help
              Print help
    ----- stderr -----
    ");
}

#[test]
fn start_already_running_exits_with_error() {
    let context = test_context!();

    let sock_dir = tempfile::tempdir_in("/tmp").unwrap();
    let bind_addr = sock_dir.path().join("test.sock");
    let bind_str = bind_addr.to_string_lossy().to_string();

    context
        .command()
        .args(["server", "start", "--dry-run", "--bind", &bind_str])
        .assert()
        .success();

    let mut filters = context.filters();
    filters.push((r"pid \d+".to_string(), "pid [PID]".to_string()));
    filters.push((regex::escape(&bind_str), "[SOCKET_PATH]".to_string()));
    let mut cmd = context.command();
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
        .args(["server", "stop"])
        .assert()
        .success();
}

#[test]
fn concurrent_autostart_converges_on_one_shared_daemon_and_cleans_up() {
    fn run_ps_json(home_dir: &std::path::Path, temp_dir: &std::path::Path, storage_dir: &std::path::Path) -> std::process::Output {
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

    let storage_dir;
    let socket_path;
    {
        let context_a = test_context!();
        let context_b = test_context!();
        assert_eq!(context_a.storage_dir, context_b.storage_dir);
        storage_dir = context_a.storage_dir.clone();
        socket_path = storage_dir.join("fabro.sock").display().to_string();

        let barrier = Arc::new(Barrier::new(3));
        let home_a = context_a.home_dir.clone();
        let temp_a = context_a.temp_dir.clone();
        let storage_a = context_a.storage_dir.clone();
        let barrier_a = Arc::clone(&barrier);
        let thread_a = std::thread::spawn(move || {
            barrier_a.wait();
            run_ps_json(&home_a, &temp_a, &storage_a)
        });

        let home_b = context_b.home_dir.clone();
        let temp_b = context_b.temp_dir.clone();
        let storage_b = context_b.storage_dir.clone();
        let barrier_b = Arc::clone(&barrier);
        let thread_b = std::thread::spawn(move || {
            barrier_b.wait();
            run_ps_json(&home_b, &temp_b, &storage_b)
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
    }

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
