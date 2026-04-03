#[cfg(feature = "server")]
use fabro_test::{fabro_snapshot, test_context};

#[test]
#[cfg(feature = "server")]
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
#[cfg(feature = "server")]
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
