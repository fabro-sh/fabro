#![expect(
    clippy::disallowed_types,
    reason = "integration test: occupies a fixed TCP port via sync std::net::TcpListener to \
              verify the server-start fallback path when the default port is unavailable"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "integration test stages server-start fixtures with sync std::fs::write"
)]

use std::process::Stdio;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use fabro_config::{Storage, envfile};
use fabro_test::{
    TestContext, apply_test_isolation, fabro_snapshot, isolated_storage_dir, server_log_files,
    test_context, wait_for_log_line, wait_for_path,
};
use fabro_util::dev_token;

const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";
const TEST_SESSION_SECRET: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn write_dev_token_server_settings(config_path: &std::path::Path, rest: &str) {
    std::fs::write(
        config_path,
        format!("_version = 1\n\n[server.auth]\nmethods = [\"dev-token\"]\n\n{rest}"),
    )
    .expect("writing dev-token server settings fixture");
}

fn provision_dev_token_auth(_home_dir: &std::path::Path, storage_dir: &std::path::Path) {
    let runtime_directory = Storage::new(storage_dir).runtime_directory();
    let server_env_path = runtime_directory.env_path();
    envfile::merge_env_file(&server_env_path, [
        ("FABRO_DEV_TOKEN", TEST_DEV_TOKEN),
        ("SESSION_SECRET", TEST_SESSION_SECRET),
    ])
    .expect("merging server auth into server.env");
    dev_token::write_dev_token(&runtime_directory.dev_token_path(), TEST_DEV_TOKEN)
        .expect("writing runtime dev-token");
}

#[derive(Clone, Copy, Debug)]
enum ServerStartMode {
    Foreground,
    Daemon,
}

impl ServerStartMode {
    const ALL: [Self; 2] = [Self::Foreground, Self::Daemon];

    fn name(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Daemon => "daemon",
        }
    }

    fn add_args(self, cmd: &mut assert_cmd::Command) {
        if matches!(self, Self::Foreground) {
            cmd.arg("--foreground");
        }
    }
}

struct StartupFailureCase {
    name:           &'static str,
    settings:       &'static str,
    server_env:     &'static [(&'static str, &'static str)],
    expected_error: &'static str,
}

fn run_startup_failure(context: &TestContext, mode: ServerStartMode, case: &StartupFailureCase) {
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root
        .path()
        .join(format!("{}-{}", case.name, mode.name()));
    let socket_path = storage_root
        .path()
        .join(format!("{}-{}.sock", case.name, mode.name()));
    let config_dir = tempfile::tempdir_in("/tmp").expect("creating startup failure config dir");
    let config_path = config_dir.path().join("settings.toml");
    std::fs::write(&config_path, case.settings).expect("writing startup failure settings");
    if !case.server_env.is_empty() {
        envfile::merge_env_file(
            &Storage::new(&storage_dir).runtime_directory().env_path(),
            case.server_env.iter().copied(),
        )
        .expect("writing startup failure server.env");
    }

    let mut cmd = context.command();
    cmd.args(["server", "start"]);
    mode.add_args(&mut cmd);
    cmd.arg("--storage-dir")
        .arg(&storage_dir)
        .arg("--bind")
        .arg(&socket_path)
        .arg("--config")
        .arg(&config_path);
    let output = cmd
        .output()
        .expect("server start failure command should run");

    assert!(
        !output.status.success(),
        "server start should reject {} in {} mode\nstdout:\n{}\nstderr:\n{}",
        case.name,
        mode.name(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "server start rejection should not write stdout for {} in {} mode:\n{}",
        case.name,
        mode.name(),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("error: {}\n", case.expected_error),
        "unexpected stderr for {} in {} mode",
        case.name,
        mode.name()
    );

    let log_path = storage_dir.join("logs/server.log");
    match mode {
        ServerStartMode::Foreground => assert!(
            log_path.exists(),
            "foreground validation intentionally runs after log bootstrap for {}",
            case.name
        ),
        ServerStartMode::Daemon => assert!(
            !log_path.exists(),
            "daemon validation should fail before creating server.log for {}",
            case.name
        ),
    }
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["server", "start", "--help"]);
    let mut filters = context.filters();
    // `--watch-web` is gated by `#[cfg(debug_assertions)]` in ServeArgs and
    // only appears in debug-build help output. Strip it so the snapshot is
    // consistent across debug and release builds.
    filters.push((
        r"(?m)^ {6}--watch-web\n {10}Run `bun run dev`.*\n".to_string(),
        String::new(),
    ));
    fabro_snapshot!(filters, cmd, @"
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
fn start_rejects_invalid_startup_configuration_in_foreground_and_daemon() {
    const DEV_TOKEN_SETTINGS: &str = r#"_version = 1

[server.auth]
methods = ["dev-token"]
"#;
    const GITHUB_SETTINGS: &str = r#"_version = 1

[server.web]
enabled = true

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = ["octocat"]

[server.integrations.github]
client_id = "Iv1.testclient"
"#;
    const GITHUB_WITHOUT_CLIENT_ID_SETTINGS: &str = r#"_version = 1

[server.web]
enabled = true

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = ["octocat"]
"#;
    const GITHUB_WEB_DISABLED_SETTINGS: &str = r#"_version = 1

[server.web]
enabled = false

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = ["octocat"]

[server.integrations.github]
client_id = "Iv1.testclient"
"#;
    const EMPTY_AUTH_METHODS_SETTINGS: &str = r"_version = 1

[server.auth]
methods = []
";

    let context = test_context!();
    let cases = [
        StartupFailureCase {
            name:           "missing-session-secret",
            settings:       DEV_TOKEN_SETTINGS,
            server_env:     &[("FABRO_DEV_TOKEN", TEST_DEV_TOKEN)],
            expected_error: "Fabro server refuses to start: auth is configured but SESSION_SECRET is not set.",
        },
        StartupFailureCase {
            name:           "missing-dev-token",
            settings:       DEV_TOKEN_SETTINGS,
            server_env:     &[("SESSION_SECRET", TEST_SESSION_SECRET)],
            expected_error: "Fabro server refuses to start: dev-token auth is enabled but FABRO_DEV_TOKEN is not set.",
        },
        StartupFailureCase {
            name:           "missing-github-client-secret",
            settings:       GITHUB_SETTINGS,
            server_env:     &[("SESSION_SECRET", TEST_SESSION_SECRET)],
            expected_error: "Fabro server refuses to start: github auth is enabled but GITHUB_APP_CLIENT_SECRET is not set.",
        },
        StartupFailureCase {
            name:           "empty-auth-methods",
            settings:       EMPTY_AUTH_METHODS_SETTINGS,
            server_env:     &[],
            expected_error: "failed to resolve server settings:\n  server.auth.methods: invalid value - must not be empty",
        },
        StartupFailureCase {
            name:           "github-web-disabled",
            settings:       GITHUB_WEB_DISABLED_SETTINGS,
            server_env:     &[
                ("SESSION_SECRET", TEST_SESSION_SECRET),
                ("GITHUB_APP_CLIENT_SECRET", "github-client-secret"),
            ],
            expected_error: "Fabro server refuses to start: github auth is enabled but server.web.enabled is false.",
        },
        StartupFailureCase {
            name:           "github-missing-client-id",
            settings:       GITHUB_WITHOUT_CLIENT_ID_SETTINGS,
            server_env:     &[
                ("SESSION_SECRET", TEST_SESSION_SECRET),
                ("GITHUB_APP_CLIENT_SECRET", "github-client-secret"),
            ],
            expected_error: "Fabro server refuses to start: github auth is enabled but server.integrations.github.client_id is not configured.",
        },
        StartupFailureCase {
            name:           "invalid-dev-token",
            settings:       DEV_TOKEN_SETTINGS,
            server_env:     &[
                ("SESSION_SECRET", TEST_SESSION_SECRET),
                ("FABRO_DEV_TOKEN", "not-a-valid-dev-token"),
            ],
            expected_error: "Fabro server refuses to start: FABRO_DEV_TOKEN has invalid format.",
        },
    ];

    for case in &cases {
        for mode in ServerStartMode::ALL {
            run_startup_failure(&context, mode, case);
        }
    }
}

#[test]
fn start_already_running_exits_with_error() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    provision_dev_token_auth(&context.home_dir, &storage_dir);

    let sock_dir = tempfile::tempdir_in("/tmp").unwrap();
    let bind_addr = sock_dir.path().join("test.sock");
    let bind_str = bind_addr.to_string_lossy().to_string();

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "start", "--bind", &bind_str])
        .assert()
        .success();

    let mut filters = context.filters();
    filters.push((r"pid \d+".to_string(), "pid [PID]".to_string()));
    filters.push((regex::escape(&bind_str), "[SOCKET_PATH]".to_string()));
    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "start", "--bind", &bind_str]);
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
#[expect(
    clippy::disallowed_methods,
    reason = "This integration test needs the real foreground process to verify install-mode startup behavior."
)]
fn start_without_default_settings_enters_install_mode_in_foreground() {
    let home_dir = tempfile::tempdir_in("/tmp").unwrap();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    apply_test_isolation(&mut cmd, home_dir.path());
    cmd.env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "start", "--bind", "127.0.0.1:0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("server start should spawn");
    std::thread::sleep(Duration::from_millis(750));
    assert!(
        child
            .try_wait()
            .expect("install-mode server should still be running")
            .is_none(),
        "install mode should run in the foreground instead of daemonizing"
    );

    child.kill().expect("kill install-mode server");
    let output = child
        .wait_with_output()
        .expect("collect install-mode stderr");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("install mode active"),
        "expected install mode banner, got: {stderr}"
    );
    assert!(
        stderr.contains("/install?token="),
        "expected install-mode URL with token, got: {stderr}"
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration test spawns the real foreground server process to verify log ownership."
)]
fn foreground_start_writes_tracing_to_storage_server_log() {
    let home_dir = tempfile::tempdir_in("/tmp").unwrap();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let socket_path = storage_root.path().join("foreground.sock");
    let config_dir = tempfile::tempdir_in("/tmp").unwrap();
    let config_path = config_dir.path().join("settings.toml");
    write_dev_token_server_settings(&config_path, "");
    provision_dev_token_auth(home_dir.path(), &storage_dir);
    let storage_log_path = storage_dir.join("logs").join("server.log");
    std::fs::create_dir_all(storage_log_path.parent().unwrap()).unwrap();
    std::fs::write(&storage_log_path, "stale pre-start log entry\n").unwrap();

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    apply_test_isolation(&mut cmd, home_dir.path());
    cmd.args(["server", "start", "--foreground"])
        .arg("--storage-dir")
        .arg(&storage_dir)
        .arg("--bind")
        .arg(&socket_path)
        .arg("--config")
        .arg(&config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("server start should spawn");
    let record_path = storage_dir.join("server.json");
    let deadline = Instant::now() + Duration::from_secs(5);

    while Instant::now() < deadline {
        if record_path.exists() {
            break;
        }
        if let Some(status) = child.try_wait().expect("server start should poll") {
            let output = child
                .wait_with_output()
                .expect("server start output should be readable");
            panic!(
                "foreground server exited before writing server.json with status {status}:\nstderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        record_path.exists(),
        "expected foreground start to create server.json"
    );

    wait_for_log_line(&storage_log_path, "API server started");

    let stop_output = {
        let mut stop = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
        apply_test_isolation(&mut stop, home_dir.path());
        stop.args(["server", "stop"])
            .arg("--storage-dir")
            .arg(&storage_dir)
            .output()
            .expect("server stop should run")
    };
    assert!(
        stop_output.status.success(),
        "server stop should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stop_output.stdout),
        String::from_utf8_lossy(&stop_output.stderr)
    );

    let output = child
        .wait_with_output()
        .expect("server start output should be readable");
    wait_for_log_line(
        &storage_log_path,
        "Shutdown signal received, stopping server",
    );
    let storage_log = std::fs::read_to_string(&storage_log_path).unwrap_or_default();
    assert!(
        storage_log.contains("API server started"),
        "expected {} to contain server tracing, got:\n{}\nforeground stderr:\n{}",
        storage_log_path.display(),
        storage_log,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        storage_log.contains("Shutdown signal received, stopping server"),
        "expected {} to contain shutdown tracing, got:\n{}",
        storage_log_path.display(),
        storage_log
    );
    assert!(
        !storage_log.contains("stale pre-start log entry"),
        "expected startup to truncate stale log contents, got:\n{storage_log}",
    );
    assert!(
        storage_log.find("API server started")
            < storage_log.find("Shutdown signal received, stopping server"),
        "expected shutdown trace to append after startup trace, got:\n{storage_log}",
    );

    let home_server_logs = server_log_files(&home_dir.path().join(".fabro").join("logs"));
    assert!(
        home_server_logs.is_empty(),
        "expected foreground server start to avoid home server logs, found: {home_server_logs:?}"
    );
}

#[test]
fn daemon_start_writes_tracing_to_storage_server_log() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let socket_path = storage_root.path().join("daemon.sock");
    let config_dir = tempfile::tempdir_in("/tmp").unwrap();
    let config_path = config_dir.path().join("settings.toml");
    write_dev_token_server_settings(&config_path, "");
    provision_dev_token_auth(&context.home_dir, &storage_dir);
    let storage_log_path = storage_dir.join("logs").join("server.log");
    std::fs::create_dir_all(storage_log_path.parent().unwrap()).unwrap();
    std::fs::write(&storage_log_path, "stale pre-start log entry\n").unwrap();

    context
        .command()
        .args(["server", "start"])
        .arg("--storage-dir")
        .arg(&storage_dir)
        .arg("--bind")
        .arg(&socket_path)
        .arg("--config")
        .arg(&config_path)
        .assert()
        .success();

    wait_for_log_line(&storage_log_path, "API server started");

    context
        .command()
        .args(["server", "stop"])
        .arg("--storage-dir")
        .arg(&storage_dir)
        .assert()
        .success();

    wait_for_log_line(
        &storage_log_path,
        "Shutdown signal received, stopping server",
    );

    let storage_log = std::fs::read_to_string(&storage_log_path).unwrap_or_default();
    assert!(
        storage_log.contains("API server started"),
        "expected {} to contain startup tracing, got:\n{}",
        storage_log_path.display(),
        storage_log
    );
    assert!(
        storage_log.contains("Shutdown signal received, stopping server"),
        "expected {} to contain shutdown tracing, got:\n{}",
        storage_log_path.display(),
        storage_log
    );
    assert!(
        !storage_log.contains("stale pre-start log entry"),
        "expected startup to truncate stale log contents, got:\n{storage_log}",
    );
    assert!(
        storage_log.find("API server started")
            < storage_log.find("Shutdown signal received, stopping server"),
        "expected shutdown trace to append after startup trace, got:\n{storage_log}",
    );

    let home_server_logs = server_log_files(&context.home_dir.join(".fabro").join("logs"));
    assert!(
        home_server_logs.is_empty(),
        "expected daemonized server start to avoid home server logs, found: {home_server_logs:?}"
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration test starts two real foreground server processes to verify lock ownership protects log truncation."
)]
fn concurrent_foreground_start_does_not_retruncate_storage_server_log() {
    let home_dir = tempfile::tempdir_in("/tmp").unwrap();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let first_socket_path = storage_root.path().join("foreground-first.sock");
    let second_socket_path = storage_root.path().join("foreground-second.sock");
    let config_dir = tempfile::tempdir_in("/tmp").unwrap();
    let config_path = config_dir.path().join("settings.toml");
    write_dev_token_server_settings(&config_path, "");
    provision_dev_token_auth(home_dir.path(), &storage_dir);

    let storage_log_path = storage_dir.join("logs").join("server.log");
    std::fs::create_dir_all(storage_log_path.parent().unwrap()).unwrap();
    std::fs::write(&storage_log_path, "stale pre-start log entry\n").unwrap();

    let mut first = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    apply_test_isolation(&mut first, home_dir.path());
    first
        .args(["server", "start", "--foreground"])
        .arg("--storage-dir")
        .arg(&storage_dir)
        .arg("--bind")
        .arg(&first_socket_path)
        .arg("--config")
        .arg(&config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let first_child = first.spawn().expect("first foreground start should spawn");
    wait_for_path(&storage_dir.join("server.json"));
    wait_for_log_line(&storage_log_path, "API server started");

    let marker = "marker-after-first-start\n";
    {
        use std::io::Write as _;

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&storage_log_path)
            .unwrap();
        file.write_all(marker.as_bytes()).unwrap();
    }

    let second_output = {
        let mut second = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
        apply_test_isolation(&mut second, home_dir.path());
        second
            .args(["server", "start", "--foreground"])
            .arg("--storage-dir")
            .arg(&storage_dir)
            .arg("--bind")
            .arg(&second_socket_path)
            .arg("--config")
            .arg(&config_path)
            .output()
            .expect("second foreground start should run")
    };

    assert!(
        !second_output.status.success(),
        "second foreground start should fail:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&second_output.stdout),
        String::from_utf8_lossy(&second_output.stderr)
    );
    let second_stderr = String::from_utf8_lossy(&second_output.stderr);
    assert!(
        second_stderr.contains("timed out waiting for server lock"),
        "expected lock timeout failure, got:\n{second_stderr}"
    );

    let storage_log = std::fs::read_to_string(&storage_log_path).unwrap_or_default();
    assert!(
        storage_log.contains(marker.trim_end()),
        "expected second start to avoid retruncating the log, got:\n{storage_log}",
    );
    assert!(
        !storage_log.contains("stale pre-start log entry"),
        "expected the first start to truncate stale log contents, got:\n{storage_log}",
    );

    let stop_output = {
        let mut stop = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
        apply_test_isolation(&mut stop, home_dir.path());
        stop.args(["server", "stop", "--timeout", "0"])
            .arg("--storage-dir")
            .arg(&storage_dir)
            .output()
            .expect("server stop should run")
    };
    assert!(
        stop_output.status.success(),
        "server stop should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stop_output.stdout),
        String::from_utf8_lossy(&stop_output.stderr)
    );

    let _ = first_child
        .wait_with_output()
        .expect("first child should exit");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration test needs the real foreground process to verify install-mode startup warnings."
)]
fn start_without_settings_ignores_no_web_during_install() {
    let home_dir = tempfile::tempdir_in("/tmp").unwrap();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    apply_test_isolation(&mut cmd, home_dir.path());
    cmd.env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "start", "--no-web", "--bind", "127.0.0.1:0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("server start should spawn");
    std::thread::sleep(Duration::from_millis(750));
    assert!(
        child
            .try_wait()
            .expect("install-mode server should still be running")
            .is_none(),
        "install mode should keep running in the foreground"
    );

    child.kill().expect("kill install-mode server");
    let output = child
        .wait_with_output()
        .expect("collect install-mode stderr");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains(
            "Warning: --no-web is ignored during install; will be respected on next start."
        ),
        "expected --no-web warning, got: {stderr}"
    );
    assert!(
        stderr.contains("install mode active"),
        "expected install mode banner, got: {stderr}"
    );
}

#[test]
fn start_with_missing_explicit_flag_config_errors_without_entering_install_mode() {
    let context = test_context!();
    let missing_config = tempfile::tempdir_in("/tmp")
        .unwrap()
        .path()
        .join("missing-settings.toml");

    let mut filters = context.filters();
    filters.push((
        regex::escape(missing_config.to_str().unwrap()),
        "[MISSING_CONFIG]".to_string(),
    ));

    let mut explicit_cmd = context.command();
    explicit_cmd.args([
        "server",
        "start",
        "--config",
        missing_config.to_str().unwrap(),
    ]);
    fabro_snapshot!(filters.clone(), explicit_cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: reading config file [MISSING_CONFIG]: No such file or directory (os error 2)
      > No such file or directory (os error 2)
    ");
}

#[test]
fn start_with_missing_env_config_errors_without_entering_install_mode() {
    let context = test_context!();
    let missing_config = tempfile::tempdir_in("/tmp")
        .unwrap()
        .path()
        .join("missing-settings.toml");

    let mut filters = context.filters();
    filters.push((
        regex::escape(missing_config.to_str().unwrap()),
        "[MISSING_CONFIG]".to_string(),
    ));

    let mut env_cmd = context.command();
    env_cmd
        .env("FABRO_CONFIG", &missing_config)
        .args(["server", "start"]);
    fabro_snapshot!(filters, env_cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: reading config file [MISSING_CONFIG]: No such file or directory (os error 2)
      > No such file or directory (os error 2)
    ");
}

#[test]
fn start_with_malformed_default_settings_errors_without_entering_install_mode() {
    let context = test_context!();
    let settings_dir = context.home_dir.join(".fabro");
    std::fs::create_dir_all(&settings_dir).unwrap();
    let settings_path = settings_dir.join("settings.toml");
    std::fs::write(&settings_path, "[server.listen\n").unwrap();

    let mut filters = context.filters();
    filters.push((
        regex::escape(settings_path.to_str().unwrap()),
        "[SETTINGS_PATH]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["server", "start"]);
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Failed to parse settings file at [HOME_DIR]/.fabro/settings.toml: settings file is not valid TOML: TOML parse error at line 1, column 15
        |
      1 | [server.listen
        |               ^
      invalid table header
      expected `.`, `]`
      > settings file is not valid TOML: TOML parse error at line 1, column 15
      >   |
      > 1 | [server.listen
      >   |               ^
      > invalid table header
      > expected `.`, `]`
    ");
}

#[test]
fn start_without_bind_uses_home_socket_instead_of_storage_socket() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    provision_dev_token_auth(&context.home_dir, &storage_dir);
    let expected_socket = context.home_dir.join(".fabro").join("fabro.sock");
    let storage_socket = storage_dir.join("fabro.sock");

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "start"])
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
fn start_without_bind_uses_configured_tcp_listen_address() {
    let context = test_context!();
    let storage_root = isolated_storage_dir();
    let storage_dir = storage_root.path().join("storage");
    let config_dir = tempfile::tempdir_in("/tmp").unwrap();
    let config_path = config_dir.path().join("settings.toml");
    write_dev_token_server_settings(
        &config_path,
        r#"[server.listen]
type = "tcp"
address = "127.0.0.1:0"
"#,
    );
    provision_dev_token_auth(&context.home_dir, &storage_dir);

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "start", "--config", config_path.to_str().unwrap()]);
    let output = cmd.output().expect("server start command should run");
    assert!(
        output.status.success(),
        "server start should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let bind_regex = regex::Regex::new(r"127\.0\.0\.1:\d+").unwrap();
    assert!(
        bind_regex.is_match(&stderr),
        "expected configured tcp bind in stderr, got {stderr}"
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
        "expected configured tcp bind, got {bind}"
    );

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
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    provision_dev_token_auth(&context.home_dir, &storage_dir);

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "start", "--bind", "127.0.0.1"]);
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
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
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
    provision_dev_token_auth(&context.home_dir, &storage_dir);

    let mut cmd = context.command();
    cmd.env("FABRO_STORAGE_DIR", &storage_dir);
    cmd.args(["server", "start", "--bind", "127.0.0.1"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Warning: TCP port 32276 is unavailable on 127.0.0.1; falling back to a random port.
    Server started (pid [PID]) on [TCP_BIND]
    Web UI: http://[TCP_BIND]
    Auth: dev-token
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
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
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
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
        fabro_test::apply_test_isolation(&mut cmd, home_dir);
        cmd.current_dir(temp_dir)
            .env("FABRO_HOME", home_dir.join(".fabro"))
            .env("FABRO_CONFIG", config_path)
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
            "_version = 1\n\n[server.auth]\nmethods = [\"dev-token\"]\n\n[server.storage]\nroot = \"{}\"\n\n[cli.target]\ntype = \"unix\"\npath = \"{}\"\n",
            storage_dir.display(),
            socket_path.display()
        ),
    )
    .unwrap();
    let home_a = tempfile::tempdir_in("/tmp").unwrap();
    let home_b = tempfile::tempdir_in("/tmp").unwrap();
    provision_dev_token_auth(home_a.path(), &storage_dir);
    provision_dev_token_auth(home_b.path(), &storage_dir);
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
