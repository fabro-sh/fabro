#![expect(
    clippy::disallowed_methods,
    reason = "CLI `server start` command: sync config/record file I/O in startup command; acquire_lock uses spawn_blocking"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use fabro_config::bind::{Bind, BindRequest};
use fabro_config::daemon::ServerDaemon;
use fabro_config::user::{FABRO_CONFIG_ENV, default_settings_path, load_settings_config};
use fabro_config::{RuntimeDirectory, envfile};
use fabro_server::jwt_auth::auth_method_name;
use fabro_server::serve::{DEFAULT_TCP_PORT, ServeArgs};
use fabro_types::settings::ServerAuthMethod;
use fabro_util::printer::Printer;
use fabro_util::session_secret;
use fabro_util::terminal::Styles;
use tokio::net::{TcpStream, UnixStream};
use tokio::process::Command as TokioCommand;
use tokio::task::spawn_blocking;
use tokio::time;

use crate::local_server;

pub(crate) struct ForegroundServerLogBootstrap {
    #[expect(dead_code, reason = "held for its Drop to release the server lock")]
    lock_file: std::fs::File,
}

pub(crate) async fn execute(
    bind: BindRequest,
    foreground: bool,
    mut serve_args: ServeArgs,
    storage_dir: PathBuf,
    foreground_log_bootstrap: Option<ForegroundServerLogBootstrap>,
    styles: &'static Styles,
    printer: Printer,
) -> Result<()> {
    serve_args.bind = Some(bind.to_string());

    if foreground {
        Box::pin(execute_foreground(
            bind,
            serve_args,
            storage_dir,
            foreground_log_bootstrap
                .context("internal error: missing foreground server log bootstrap")?,
            styles,
            printer,
        ))
        .await
    } else {
        execute_daemon(
            &bind,
            &serve_args,
            &storage_dir,
            true,
            Some(styles),
            printer,
        )
        .await
    }
}

pub(crate) async fn prepare_foreground_server_log(
    runtime_directory: &RuntimeDirectory,
) -> Result<ForegroundServerLogBootstrap> {
    let lock_file = acquire_lock(runtime_directory).await?;
    if let Some(existing) = ServerDaemon::load_running(runtime_directory)? {
        bail!(
            "Server already running (pid {}) on {}",
            existing.pid,
            existing.bind
        );
    }

    let log_path = runtime_directory.log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating log directory {}", parent.display()))?;
    }
    std::fs::File::create(&log_path)
        .with_context(|| format!("creating server log file {}", log_path.display()))?;

    Ok(ForegroundServerLogBootstrap { lock_file })
}

pub(crate) async fn ensure_server_running_for_storage(
    storage_dir: &Path,
    config_path: &Path,
) -> Result<Bind> {
    ensure_storage_server_autostart_allowed(
        std::env::var_os(FABRO_CONFIG_ENV).as_deref(),
        config_path,
        &default_settings_path(),
    )?;
    ensure_server_running_with_bind(None, config_path, storage_dir).await
}

pub(crate) async fn ensure_server_running_on_socket(
    socket_path: &Path,
    config_path: &Path,
    storage_dir: &Path,
) -> Result<Bind> {
    ensure_server_running_with_bind(
        Some(BindRequest::Unix(socket_path.to_path_buf())),
        config_path,
        storage_dir,
    )
    .await
}

async fn ensure_server_running_with_bind(
    bind_request: Option<BindRequest>,
    config_path: &Path,
    storage_dir: &Path,
) -> Result<Bind> {
    let runtime_directory = RuntimeDirectory::new(storage_dir);
    if let Some(existing) = ServerDaemon::load_running(&runtime_directory)? {
        if bind_request
            .as_ref()
            .is_none_or(|requested| bind_matches_request(&existing.bind, requested))
        {
            return Ok(existing.bind);
        }
        bail!(
            "Server already running (pid {}) on {}",
            existing.pid,
            existing.bind
        );
    }

    let serve_args = ServeArgs {
        bind: bind_request.as_ref().map(ToString::to_string),
        web: false,
        no_web: false,
        model: None,
        provider: None,
        sandbox: None,
        max_concurrent_runs: server_max_concurrent_runs_override(),
        config: Some(config_path.to_path_buf()),
        #[cfg(debug_assertions)]
        watch_web: false,
    };

    let bind_request = if let Some(bind_request) = bind_request {
        bind_request
    } else {
        let settings = load_settings_config(Some(config_path))?;
        local_server::bind_request(&settings, None)?
    };

    match execute_daemon(
        &bind_request,
        &serve_args,
        storage_dir,
        false,
        None,
        Printer::Silent,
    )
    .await
    {
        Ok(()) => ServerDaemon::load_running(&runtime_directory)?
            .map(|server| server.bind)
            .ok_or_else(|| {
                anyhow!(
                    "Server started but no active record was found for {}",
                    storage_dir.display()
                )
            }),
        Err(err) => {
            if let Some(existing) = ServerDaemon::load_running(&runtime_directory)? {
                Ok(existing.bind)
            } else {
                Err(err)
            }
        }
    }
}

fn ensure_storage_server_autostart_allowed(
    config_env: Option<&std::ffi::OsStr>,
    config_path: &Path,
    default_settings_path: &Path,
) -> Result<()> {
    if config_env.is_none() && config_path == default_settings_path && !config_path.exists() {
        bail!(
            "Cannot reach Fabro server: no settings.toml configured.\n\nRun one of:\n  fabro server start    # browser-based wizard\n  fabro install         # terminal wizard"
        );
    }
    Ok(())
}

fn bind_matches_request(existing: &Bind, requested: &BindRequest) -> bool {
    match (existing, requested) {
        (Bind::Unix(existing_path), BindRequest::Unix(requested_path)) => {
            existing_path == requested_path
        }
        (Bind::Tcp(existing_addr), BindRequest::Tcp(requested_addr)) => {
            existing_addr == requested_addr
        }
        (Bind::Tcp(existing_addr), BindRequest::TcpHost(requested_host)) => {
            existing_addr.ip() == *requested_host
        }
        _ => false,
    }
}

fn server_max_concurrent_runs_override() -> Option<usize> {
    std::env::var("FABRO_SERVER_MAX_CONCURRENT_RUNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn configured_auth_methods(config_path: Option<&Path>) -> Vec<ServerAuthMethod> {
    load_settings_config(config_path)
        .ok()
        .map(|settings| local_server::auth_methods(&settings))
        .unwrap_or_default()
}

fn valid_session_secret(secret: &str) -> bool {
    session_secret::validate_session_secret(secret).is_ok()
}

fn load_or_create_local_session_secret(runtime_directory: &RuntimeDirectory) -> Result<String> {
    if let Some(secret) = std::env::var("SESSION_SECRET")
        .ok()
        .filter(|secret| valid_session_secret(secret))
    {
        return Ok(secret);
    }

    let server_env_path = runtime_directory.env_path();
    if let Some(secret) = envfile::read_env_file(&server_env_path)
        .ok()
        .and_then(|entries| entries.get("SESSION_SECRET").cloned())
        .filter(|secret| valid_session_secret(secret))
    {
        return Ok(secret);
    }

    let secret = session_secret::generate_session_secret();
    envfile::merge_env_file(&server_env_path, [("SESSION_SECRET", secret.as_str())])
        .with_context(|| format!("merging session secret into {}", server_env_path.display()))?;
    Ok(secret)
}

// ---------------------------------------------------------------------------
// Foreground mode
// ---------------------------------------------------------------------------

async fn execute_foreground(
    bind: BindRequest,
    serve_args: ServeArgs,
    storage_dir: PathBuf,
    _log_bootstrap: ForegroundServerLogBootstrap,
    styles: &'static Styles,
    _printer: Printer,
) -> Result<()> {
    let session_secret = load_or_create_local_session_secret(&RuntimeDirectory::new(&storage_dir))?;
    let prior_session_secret = std::env::var_os("SESSION_SECRET");
    std::env::set_var("SESSION_SECRET", &session_secret);
    let _env_guard =
        scopeguard::guard(
            prior_session_secret,
            |prior_session_secret| match prior_session_secret {
                Some(value) => std::env::set_var("SESSION_SECRET", value),
                None => std::env::remove_var("SESSION_SECRET"),
            },
        );

    super::foreground::serve_with_daemon_record(serve_args, bind, storage_dir, styles).await
}

// ---------------------------------------------------------------------------
// Daemon mode
// ---------------------------------------------------------------------------

async fn execute_daemon(
    bind: &BindRequest,
    serve_args: &ServeArgs,
    storage_dir: &Path,
    announce: bool,
    styles: Option<&Styles>,
    printer: Printer,
) -> Result<()> {
    let runtime_directory = RuntimeDirectory::new(storage_dir);
    let lock_file = acquire_lock(&runtime_directory).await?;
    let _lock_file = lock_file;

    if let Some(existing) = ServerDaemon::load_running(&runtime_directory)? {
        if announce {
            bail!(
                "Server already running (pid {}) on {}",
                existing.pid,
                existing.bind
            );
        }
        return Ok(());
    }

    let log_path = runtime_directory.log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating log directory {}", parent.display()))?;
    }

    let log_file = std::fs::File::create(&log_path)
        .with_context(|| format!("creating server log file {}", log_path.display()))?;
    let stdout_log = log_file
        .try_clone()
        .with_context(|| format!("cloning server log file handle for {}", log_path.display()))?;
    let exe = std::env::current_exe().context("resolving current fabro executable path")?;

    let mut cmd = TokioCommand::new(&exe);
    cmd.args(["server", "__serve"])
        .arg("--bind")
        .arg(bind.to_string());

    if let Some(ref model) = serve_args.model {
        cmd.args(["--model", model]);
    }
    if let Some(ref provider) = serve_args.provider {
        cmd.args(["--provider", provider]);
    }
    if serve_args.web {
        cmd.arg("--web");
    }
    if serve_args.no_web {
        cmd.arg("--no-web");
    }
    if let Some(ref sandbox) = serve_args.sandbox {
        cmd.args(["--sandbox", &sandbox.to_string()]);
    }
    if let Some(max) = serve_args.max_concurrent_runs {
        cmd.args(["--max-concurrent-runs", &max.to_string()]);
    }
    if let Some(ref config) = serve_args.config {
        cmd.arg("--config").arg(config);
    }
    #[cfg(debug_assertions)]
    if serve_args.watch_web {
        cmd.arg("--watch-web");
    }

    let session_secret = load_or_create_local_session_secret(&runtime_directory)?;
    cmd.arg("--storage-dir").arg(storage_dir);
    cmd.env("SESSION_SECRET", &session_secret);

    cmd.env_remove("FABRO_JSON");
    cmd.stdout(stdout_log)
        .stderr(log_file)
        .stdin(std::process::Stdio::null());

    #[cfg(unix)]
    fabro_proc::pre_exec_setsid(cmd.as_std_mut());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning fabro server subprocess {}", exe.display()))?;

    if let Ok(Some(status)) = child.try_wait() {
        ServerDaemon::remove(&runtime_directory);
        let tail = read_log_tail(&log_path, 20);
        if !tail.is_empty() {
            fabro_util::printerr!(printer, "{tail}");
        }
        bail!("Server exited immediately with status {status}");
    }

    let poll_interval = Duration::from_millis(50);
    let timeout = Duration::from_secs(5);
    let mut elapsed = Duration::ZERO;

    while elapsed < timeout {
        let daemon = match ServerDaemon::read(&runtime_directory) {
            Ok(daemon) => daemon,
            Err(err) => {
                ServerDaemon::remove(&runtime_directory);
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(err);
            }
        };
        if let Some(daemon) = daemon {
            if try_connect(&daemon.bind).await {
                if announce {
                    let pid = child.id().unwrap_or_default();
                    maybe_warn_host_port_fallback(bind, &daemon.bind, printer);
                    fabro_util::printerr!(
                        printer,
                        "Server started (pid {}) on {}",
                        pid,
                        daemon.bind
                    );
                    if let Bind::Tcp(addr) = &daemon.bind {
                        let url = format!("http://{addr}");
                        let styled = match styles {
                            Some(s) => format!("{}", s.cyan.apply_to(&url)),
                            None => url,
                        };
                        fabro_util::printerr!(printer, "Web UI: {styled}");
                    }
                    print_auth_methods(printer, serve_args);
                }
                return Ok(());
            }
        }

        if let Ok(Some(status)) = child.try_wait() {
            ServerDaemon::remove(&runtime_directory);
            let tail = read_log_tail(&log_path, 20);
            if !tail.is_empty() {
                fabro_util::printerr!(printer, "{tail}");
            }
            bail!("Server exited during startup with status {status}");
        }

        time::sleep(poll_interval).await;
        elapsed += poll_interval;
    }

    ServerDaemon::remove(&runtime_directory);
    let _ = child.kill().await;
    let _ = child.wait().await;
    let tail = read_log_tail(&log_path, 20);
    if !tail.is_empty() {
        fabro_util::printerr!(printer, "{tail}");
    }
    bail!("Server did not become ready within {timeout:?}");
}

fn print_auth_methods(printer: Printer, serve_args: &ServeArgs) {
    let auth_methods = configured_auth_methods(serve_args.config.as_deref());
    let names: Vec<&str> = auth_methods.iter().map(|m| auth_method_name(*m)).collect();
    fabro_util::printerr!(printer, "Auth: {}", names.join(", "));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn acquire_lock(runtime_directory: &RuntimeDirectory) -> Result<std::fs::File> {
    let lock_path = runtime_directory.lock_path();
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating server lock directory {}", parent.display()))?;
    }
    let lock_path_for_open = lock_path.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "OpenOptions::open inside spawn_blocking; file-lock semantics need a real \
                  std::fs::File handle for fabro_proc::try_flock_exclusive"
    )]
    let lock_file = spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path_for_open)
    })
    .await
    .context("lock-file open task failed")?
    .with_context(|| format!("opening server lock file {}", lock_path.display()))?;

    let poll_interval = Duration::from_millis(50);
    let timeout = Duration::from_secs(5);
    let mut elapsed = Duration::ZERO;

    while !fabro_proc::try_flock_exclusive(&lock_file)? {
        if elapsed >= timeout {
            bail!("timed out waiting for server lock");
        }
        time::sleep(poll_interval).await;
        elapsed += poll_interval;
    }

    Ok(lock_file)
}

async fn try_connect(bind: &Bind) -> bool {
    let connect_timeout = Duration::from_millis(100);
    match bind {
        Bind::Tcp(addr) => time::timeout(connect_timeout, TcpStream::connect(addr))
            .await
            .is_ok_and(|r| r.is_ok()),
        Bind::Unix(path) => time::timeout(connect_timeout, UnixStream::connect(path))
            .await
            .is_ok_and(|r| r.is_ok()),
    }
}

fn maybe_warn_host_port_fallback(requested: &BindRequest, resolved: &Bind, printer: Printer) {
    let BindRequest::TcpHost(host) = requested else {
        return;
    };
    let Bind::Tcp(addr) = resolved else {
        return;
    };
    if addr.ip() == *host && addr.port() != DEFAULT_TCP_PORT {
        fabro_util::printerr!(
            printer,
            "Warning: TCP port {DEFAULT_TCP_PORT} is unavailable on {host}; falling back to a random port."
        );
    }
}

fn read_log_tail(log_path: &Path, lines: usize) -> String {
    match std::fs::read_to_string(log_path) {
        Ok(content) => {
            let tail: Vec<&str> = content.lines().rev().take(lines).collect();
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        }
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use fabro_util::Home;

    use super::ensure_storage_server_autostart_allowed;

    #[test]
    fn ensure_server_running_for_storage_errors_when_install_mode_is_required() {
        let temp_home = tempfile::tempdir().unwrap();
        let default_settings_path = Home::new(temp_home.path()).user_config();
        let result = ensure_storage_server_autostart_allowed(
            None,
            &default_settings_path,
            &default_settings_path,
        );

        let err =
            result.expect_err("missing default settings.toml should not auto-start install mode");
        let message = err.to_string();
        assert!(
            message.contains("Cannot reach Fabro server: no settings.toml configured."),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("fabro server start"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("fabro install"),
            "unexpected error: {message}"
        );
    }
}
