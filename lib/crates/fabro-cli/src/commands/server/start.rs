use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use fabro_config::user::{FABRO_CONFIG_ENV, default_settings_path, load_settings_config};
use fabro_config::{Storage, envfile, resolve_server_from_file};
use fabro_server::bind::{Bind, BindRequest};
use fabro_server::jwt_auth::auth_method_name;
use fabro_server::serve;
use fabro_server::serve::{DEFAULT_TCP_PORT, ServeArgs};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_util::{Home, dev_token, session_secret};
use tokio::net::{TcpStream, UnixStream};
use tokio::process::Command as TokioCommand;
use tokio::time;

use super::record;

pub(crate) async fn execute(
    bind: BindRequest,
    foreground: bool,
    mut serve_args: ServeArgs,
    storage_dir: PathBuf,
    styles: &'static Styles,
    printer: Printer,
) -> Result<()> {
    serve_args.bind = Some(bind.to_string());

    if foreground {
        Box::pin(execute_foreground(
            bind,
            serve_args,
            storage_dir,
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
) -> Result<()> {
    let _ = ensure_server_running_with_bind(
        Some(BindRequest::Unix(socket_path.to_path_buf())),
        config_path,
        storage_dir,
    )
    .await?;
    Ok(())
}

async fn ensure_server_running_with_bind(
    bind_request: Option<BindRequest>,
    config_path: &Path,
    storage_dir: &Path,
) -> Result<Bind> {
    if let Some(existing) = record::active_server_record(storage_dir) {
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
        serve::resolve_bind_request_from_settings(&settings, None)?
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
        Ok(()) => record::active_server_record(storage_dir)
            .map(|server| server.bind)
            .ok_or_else(|| {
                anyhow!(
                    "Server started but no active record was found for {}",
                    storage_dir.display()
                )
            }),
        Err(err) => {
            if let Some(existing) = record::active_server_record(storage_dir) {
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

fn load_or_create_local_dev_token(storage_dir: &Path, home: &Home) -> Result<String> {
    if let Some(token) = std::env::var("FABRO_DEV_TOKEN")
        .ok()
        .filter(|token| dev_token::validate_dev_token_format(token))
    {
        return Ok(token);
    }

    let storage = Storage::new(storage_dir);
    let server_env_path = storage.server_state().env_path();
    if let Some(token) = envfile::read_env_file(&server_env_path)
        .ok()
        .and_then(|entries| entries.get("FABRO_DEV_TOKEN").cloned())
        .filter(|token| dev_token::validate_dev_token_format(token))
    {
        dev_token::write_dev_token(&home.dev_token_path(), &token)
            .with_context(|| format!("writing dev token to {}", home.dev_token_path().display()))?;
        dev_token::write_dev_token(&storage.server_state().dev_token_path(), &token).with_context(
            || {
                format!(
                    "writing dev token to {}",
                    storage.server_state().dev_token_path().display()
                )
            },
        )?;
        return Ok(token);
    }

    let token = dev_token::load_or_create_dev_token(&home.dev_token_path()).with_context(|| {
        format!(
            "loading or creating dev token at {}",
            home.dev_token_path().display()
        )
    })?;
    dev_token::write_dev_token(&storage.server_state().dev_token_path(), &token).with_context(
        || {
            format!(
                "writing dev token to {}",
                storage.server_state().dev_token_path().display()
            )
        },
    )?;
    Ok(token)
}

fn valid_session_secret(secret: &str) -> bool {
    session_secret::validate_session_secret(secret).is_ok()
}

fn load_or_create_local_session_secret(storage_dir: &Path) -> Result<String> {
    if let Some(secret) = std::env::var("SESSION_SECRET")
        .ok()
        .filter(|secret| valid_session_secret(secret))
    {
        return Ok(secret);
    }

    let storage = Storage::new(storage_dir);
    let server_env_path = storage.server_state().env_path();
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
    styles: &'static Styles,
    printer: Printer,
) -> Result<()> {
    let home = Home::from_env();
    let token = load_or_create_local_dev_token(&storage_dir, &home)?;
    let session_secret = load_or_create_local_session_secret(&storage_dir)?;
    let prior_token = std::env::var_os("FABRO_DEV_TOKEN");
    let prior_session_secret = std::env::var_os("SESSION_SECRET");
    std::env::set_var("FABRO_DEV_TOKEN", &token);
    std::env::set_var("SESSION_SECRET", &session_secret);
    let _env_guard = scopeguard::guard(
        (prior_token, prior_session_secret),
        |(prior_token, prior_session_secret)| {
            match prior_token {
                Some(value) => std::env::set_var("FABRO_DEV_TOKEN", value),
                None => std::env::remove_var("FABRO_DEV_TOKEN"),
            }
            match prior_session_secret {
                Some(value) => std::env::set_var("SESSION_SECRET", value),
                None => std::env::remove_var("SESSION_SECRET"),
            }
        },
    );

    let lock_file = acquire_lock(&storage_dir).await?;
    let _lock_file = lock_file; // keep alive for the duration

    if let Some(existing) = record::active_server_record(&storage_dir) {
        bail!(
            "Server already running (pid {}) on {}",
            existing.pid,
            existing.bind
        );
    }

    let server_state = Storage::new(&storage_dir).server_state();
    let record_path = server_state.record_path();
    let log_path = server_state.log_path();
    let pid = std::process::id();

    let _record_guard = scopeguard::guard(record_path.clone(), |path| {
        record::remove_server_record(&path);
    });

    let _socket_guard = if let BindRequest::Unix(ref path) = bind {
        let path = path.clone();
        Some(scopeguard::guard(path, |p| {
            let _ = std::fs::remove_file(p);
        }))
    } else {
        None
    };

    Box::pin(serve::serve_command(
        serve_args,
        styles,
        Some(storage_dir),
        move |resolved_bind| {
            print_dev_token(printer, &home, &token);
            record::write_server_record(&record_path, &record::ServerRecord {
                pid,
                bind: resolved_bind.clone(),
                log_path: log_path.clone(),
                dev_token_path: Some(home.dev_token_path()),
                started_at: Utc::now(),
            })
        },
    ))
    .await
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
    let lock_file = acquire_lock(storage_dir).await?;
    let _lock_file = lock_file; // keep alive until function returns

    if let Some(existing) = record::active_server_record(storage_dir) {
        if announce {
            bail!(
                "Server already running (pid {}) on {}",
                existing.pid,
                existing.bind
            );
        }
        return Ok(());
    }

    let server_state = Storage::new(storage_dir).server_state();
    let log_path = server_state.log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating log directory {}", parent.display()))?;
    }

    let record_path = server_state.record_path();
    let log_file = std::fs::File::create(&log_path)
        .with_context(|| format!("creating server log file {}", log_path.display()))?;
    let stdout_log = log_file
        .try_clone()
        .with_context(|| format!("cloning server log file handle for {}", log_path.display()))?;
    let exe = std::env::current_exe().context("resolving current fabro executable path")?;

    let mut cmd = TokioCommand::new(&exe);
    cmd.args(["server", "__serve"])
        .arg("--record-path")
        .arg(&record_path)
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

    let home = Home::from_env();
    let token = load_or_create_local_dev_token(storage_dir, &home)?;
    let session_secret = load_or_create_local_session_secret(storage_dir)?;
    cmd.arg("--storage-dir").arg(storage_dir);
    cmd.env("FABRO_DEV_TOKEN", &token);
    cmd.env("FABRO_DEV_TOKEN_PATH", home.dev_token_path());
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
        record::remove_server_record(&record_path);
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
        if let Some(record) = record::read_server_record(&record_path) {
            if try_connect(&record.bind).await {
                if announce {
                    let pid = child.id().unwrap_or_default();
                    maybe_warn_host_port_fallback(bind, &record.bind, printer);
                    fabro_util::printerr!(
                        printer,
                        "Server started (pid {}) on {}",
                        pid,
                        record.bind
                    );
                    if let Bind::Tcp(addr) = &record.bind {
                        let url = format!("http://{addr}");
                        let styled = match styles {
                            Some(s) => format!("{}", s.cyan.apply_to(&url)),
                            None => url,
                        };
                        fabro_util::printerr!(printer, "Web UI: {styled}");
                    }
                    print_auth_methods(printer, serve_args);
                    print_dev_token(printer, &home, &token);
                }
                return Ok(());
            }
        }

        if let Ok(Some(status)) = child.try_wait() {
            record::remove_server_record(&record_path);
            let tail = read_log_tail(&log_path, 20);
            if !tail.is_empty() {
                fabro_util::printerr!(printer, "{tail}");
            }
            bail!("Server exited during startup with status {status}");
        }

        time::sleep(poll_interval).await;
        elapsed += poll_interval;
    }

    record::remove_server_record(&record_path);
    let _ = child.kill().await;
    let _ = child.wait().await;
    let tail = read_log_tail(&log_path, 20);
    if !tail.is_empty() {
        fabro_util::printerr!(printer, "{tail}");
    }
    bail!("Server did not become ready within {timeout:?}");
}

fn print_auth_methods(printer: Printer, serve_args: &ServeArgs) {
    let settings = load_settings_config(serve_args.config.as_deref()).ok();
    let auth_methods = settings
        .as_ref()
        .and_then(|s| resolve_server_from_file(s).ok())
        .map(|s| s.auth.methods)
        .unwrap_or_default();
    let names: Vec<&str> = auth_methods.iter().map(|m| auth_method_name(*m)).collect();
    fabro_util::printerr!(printer, "Auth: {}", names.join(", "));
}

fn print_dev_token(printer: Printer, home: &Home, token: &str) {
    fabro_util::printerr!(printer, "Dev token: {token}");
    fabro_util::printerr!(printer, "Token file: {}", home.dev_token_path().display());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn acquire_lock(storage_dir: &Path) -> Result<std::fs::File> {
    let lock_path = Storage::new(storage_dir).server_state().lock_path();
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating server lock directory {}", parent.display()))?;
    }
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
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
