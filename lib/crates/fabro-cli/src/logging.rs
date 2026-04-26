#![expect(
    clippy::disallowed_methods,
    reason = "CLI logging setup: sync directory scan during startup"
)]
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fabro_static::EnvVars;
use fabro_types::settings::server::LogDestination;
use fabro_util::run_log::BufferedFileAppender;
use tracing_appender::rolling;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const LOG_RETENTION_DAYS: u32 = 7;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LogSink {
    File(PathBuf),
    Stdout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InternalLogSink {
    Cli,
    Server {
        log: LogSink,
    },
    Worker {
        server_log:       LogSink,
        per_run_log_path: PathBuf,
    },
}

pub(crate) fn init_tracing(
    debug: bool,
    config_log_level: Option<&str>,
    sink: &InternalLogSink,
) -> Result<()> {
    let default_level = if debug {
        "debug"
    } else {
        config_log_level.unwrap_or("info")
    };
    let filter = EnvFilter::try_from_env(EnvVars::FABRO_LOG)
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    match sink {
        InternalLogSink::Cli => {
            let log_dir = fabro_util::Home::from_env().logs_dir();

            std::fs::create_dir_all(&log_dir).with_context(|| {
                format!("Failed to create log directory: {}", log_dir.display())
            })?;

            let file_appender = rolling::RollingFileAppender::builder()
                .rotation(rolling::Rotation::DAILY)
                .filename_prefix("cli")
                .filename_suffix("log")
                .build(&log_dir)
                .with_context(|| "Failed to create log file appender")?;

            cleanup_old_logs(&log_dir, "cli", LOG_RETENTION_DAYS);
            init_subscriber(filter, file_appender);
        }
        InternalLogSink::Server {
            log: LogSink::File(path),
        } => {
            init_subscriber(filter, open_buffered_appender(path)?);
        }
        InternalLogSink::Server {
            log: LogSink::Stdout,
        } => {
            init_subscriber(filter, std::io::stdout);
        }
        InternalLogSink::Worker {
            server_log: LogSink::File(server_log_path),
            per_run_log_path,
        } => {
            init_worker_subscriber(
                filter,
                open_buffered_appender(server_log_path)?,
                open_buffered_appender(per_run_log_path)?,
            );
        }
        InternalLogSink::Worker {
            server_log: LogSink::Stdout,
            per_run_log_path,
        } => {
            init_worker_subscriber(
                filter,
                std::io::stdout,
                open_buffered_appender(per_run_log_path)?,
            );
        }
    }

    Ok(())
}

pub(crate) fn resolve_log_destination(
    config_destination: LogDestination,
) -> Result<LogDestination> {
    let env_value = std::env::var(EnvVars::FABRO_LOG_DESTINATION).ok();
    resolve_log_destination_with_env(config_destination, env_value.as_deref())
}

pub(crate) fn resolve_log_destination_with_env(
    config_destination: LogDestination,
    env_value: Option<&str>,
) -> Result<LogDestination> {
    match env_value {
        Some(value) => value.parse::<LogDestination>().with_context(|| {
            format!(
                "invalid {} value `{value}`; expected `file` or `stdout`",
                EnvVars::FABRO_LOG_DESTINATION
            )
        }),
        None => Ok(config_destination),
    }
}

fn cleanup_old_logs(log_dir: &Path, prefix: &str, max_age_days: u32) {
    let cutoff = chrono::Utc::now().date_naive() - chrono::Duration::days(i64::from(max_age_days));
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };

    let date_prefix = format!("{prefix}.");
    let date_suffix = ".log";

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };

        let Some(rest) = name.strip_prefix(&date_prefix) else {
            continue;
        };
        let Some(date_str) = rest.strip_suffix(date_suffix) else {
            continue;
        };

        let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
            continue;
        };

        if date < cutoff {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn init_subscriber<W>(filter: EnvFilter, file_writer: W)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(file_writer)
                .with_target(true)
                .with_ansi(false),
        )
        .init();
}

fn init_worker_subscriber<ServerWriter, RunWriter>(
    filter: EnvFilter,
    server_writer: ServerWriter,
    run_writer: RunWriter,
) where
    ServerWriter: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
    RunWriter: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(server_writer)
                .with_target(true)
                .with_ansi(false),
        )
        .with(
            fmt::layer()
                .with_writer(run_writer)
                .with_target(true)
                .with_ansi(false),
        )
        .init();
}

fn open_buffered_appender(path: &Path) -> Result<BufferedFileAppender> {
    BufferedFileAppender::open(path)
        .with_context(|| format!("Failed to open log file: {}", path.display()))
}
