#![expect(
    clippy::disallowed_methods,
    reason = "CLI logging setup: sync directory scan during startup"
)]
use std::path::Path;

use anyhow::{Context, Result};
use fabro_static::EnvVars;
use fabro_util::run_log::BufferedFileAppender;
use tracing_appender::rolling;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const LOG_RETENTION_DAYS: u32 = 7;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InternalLogSink {
    Cli,
    Server {
        path: std::path::PathBuf,
    },
    Worker {
        server_log_path:  std::path::PathBuf,
        per_run_log_path: std::path::PathBuf,
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
        InternalLogSink::Server { path } => {
            init_subscriber(filter, open_buffered_appender(path)?);
        }
        InternalLogSink::Worker {
            server_log_path,
            per_run_log_path,
        } => {
            init_worker_subscriber(
                filter,
                open_buffered_appender(server_log_path)?,
                open_buffered_appender(per_run_log_path)?,
            );
        }
    }

    Ok(())
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
