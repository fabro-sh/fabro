use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::Utc;
use fabro_server::bind::Bind;
use fabro_server::serve;
use fabro_server::serve::ServeArgs;
use fabro_util::terminal::Styles;

use super::record;

pub(crate) async fn execute(
    bind: Bind,
    foreground: bool,
    mut serve_args: ServeArgs,
    storage_dir: PathBuf,
    styles: &'static Styles,
) -> Result<()> {
    serve_args.bind = Some(bind.to_string());

    if foreground {
        execute_foreground(bind, serve_args, storage_dir, styles).await
    } else {
        execute_daemon(&bind, &serve_args, &storage_dir, true)
    }
}

pub(crate) fn ensure_server_running(storage_dir: &Path) -> Result<Bind> {
    if let Some(existing) = record::active_server_record(storage_dir) {
        return Ok(existing.bind);
    }

    let bind = Bind::Unix(storage_dir.join("fabro.sock"));
    let serve_args = ServeArgs {
        bind: None,
        model: None,
        provider: None,
        dry_run: false,
        sandbox: None,
        max_concurrent_runs: server_max_concurrent_runs_override(),
        config: None,
    };

    match execute_daemon(&bind, &serve_args, storage_dir, false) {
        Ok(()) => Ok(bind),
        Err(err) => {
            if let Some(existing) = record::active_server_record(storage_dir) {
                Ok(existing.bind)
            } else {
                Err(err)
            }
        }
    }
}

fn server_max_concurrent_runs_override() -> Option<usize> {
    std::env::var("FABRO_SERVER_MAX_CONCURRENT_RUNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

// ---------------------------------------------------------------------------
// Foreground mode
// ---------------------------------------------------------------------------

async fn execute_foreground(
    bind: Bind,
    serve_args: ServeArgs,
    storage_dir: PathBuf,
    styles: &'static Styles,
) -> Result<()> {
    let lock_file = acquire_lock(&storage_dir)?;
    let _lock_file = lock_file; // keep alive for the duration

    if let Some(existing) = record::active_server_record(&storage_dir) {
        bail!(
            "Server already running (pid {}) on {}",
            existing.pid,
            existing.bind
        );
    }

    let record_path = record::server_record_path(&storage_dir);
    record::write_server_record(
        &record_path,
        &record::ServerRecord {
            pid: std::process::id(),
            bind: bind.clone(),
            log_path: record::server_log_path(&storage_dir),
            started_at: Utc::now(),
        },
    )?;

    let _record_guard = scopeguard::guard(record_path, |path| {
        record::remove_server_record(&path);
    });

    let _socket_guard = if let Bind::Unix(ref path) = bind {
        let path = path.clone();
        Some(scopeguard::guard(path, |p| {
            let _ = std::fs::remove_file(p);
        }))
    } else {
        None
    };

    serve::serve_command(serve_args, styles, Some(storage_dir)).await
}

// ---------------------------------------------------------------------------
// Daemon mode
// ---------------------------------------------------------------------------

fn execute_daemon(
    bind: &Bind,
    serve_args: &ServeArgs,
    storage_dir: &Path,
    announce: bool,
) -> Result<()> {
    let lock_file = acquire_lock(storage_dir)?;
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

    // Rotate logs
    let log_path = record::server_log_path(storage_dir);
    let prev_path = log_path.with_extension("log.prev");
    let _ = std::fs::rename(&log_path, &prev_path);

    let record_path = record::server_record_path(storage_dir);
    let log_file = std::fs::File::create(&log_path)?;
    let stdout_log = log_file.try_clone()?;
    let exe = std::env::current_exe()?;

    let mut cmd = std::process::Command::new(&exe);
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
    if serve_args.dry_run {
        cmd.arg("--dry-run");
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

    cmd.arg("--storage-dir").arg(storage_dir);
    if matches!(bind, Bind::Unix(_)) {
        cmd.env("FABRO_LOCAL_NO_AUTH", "1");
    }

    cmd.env_remove("FABRO_JSON");
    cmd.stdout(stdout_log)
        .stderr(log_file)
        .stdin(std::process::Stdio::null());

    #[cfg(unix)]
    fabro_proc::pre_exec_setsid(&mut cmd);

    let mut child = cmd.spawn()?;

    record::write_server_record(
        &record_path,
        &record::ServerRecord {
            pid: child.id(),
            bind: bind.clone(),
            log_path: log_path.clone(),
            started_at: Utc::now(),
        },
    )?;

    if let Ok(Some(status)) = child.try_wait() {
        record::remove_server_record(&record_path);
        let tail = read_log_tail(&log_path, 20);
        if !tail.is_empty() {
            eprintln!("{tail}");
        }
        bail!("Server exited immediately with status {status}");
    }

    let poll_interval = Duration::from_millis(50);
    let timeout = Duration::from_secs(5);
    let mut elapsed = Duration::ZERO;

    while elapsed < timeout {
        if try_connect(bind) {
            if announce {
                eprintln!("Server started (pid {}) on {bind}", child.id());
            }
            return Ok(());
        }

        if let Ok(Some(status)) = child.try_wait() {
            record::remove_server_record(&record_path);
            if let Bind::Unix(ref path) = *bind {
                let _ = std::fs::remove_file(path);
            }
            let tail = read_log_tail(&log_path, 20);
            if !tail.is_empty() {
                eprintln!("{tail}");
            }
            bail!("Server exited during startup with status {status}");
        }

        thread::sleep(poll_interval);
        elapsed += poll_interval;
    }

    record::remove_server_record(&record_path);
    if let Bind::Unix(ref path) = *bind {
        let _ = std::fs::remove_file(path);
    }
    let _ = child.kill();
    let _ = child.wait();
    let tail = read_log_tail(&log_path, 20);
    if !tail.is_empty() {
        eprintln!("{tail}");
    }
    bail!("Server did not become ready within {timeout:?}");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn acquire_lock(storage_dir: &Path) -> Result<std::fs::File> {
    let lock_path = record::server_lock_path(storage_dir);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;

    let poll_interval = Duration::from_millis(50);
    let timeout = Duration::from_secs(5);
    let mut elapsed = Duration::ZERO;

    while !fabro_proc::try_flock_exclusive(&lock_file)? {
        if elapsed >= timeout {
            bail!("timed out waiting for server lock");
        }
        thread::sleep(poll_interval);
        elapsed += poll_interval;
    }

    Ok(lock_file)
}

fn try_connect(bind: &Bind) -> bool {
    match bind {
        Bind::Tcp(addr) => {
            std::net::TcpStream::connect_timeout(addr, Duration::from_millis(100)).is_ok()
        }
        Bind::Unix(path) => std::os::unix::net::UnixStream::connect(path).is_ok(),
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
