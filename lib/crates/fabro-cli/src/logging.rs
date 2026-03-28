use anyhow::{Context, Result};
use fabro_util::run_log;
use tracing_appender::rolling;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub(crate) fn init_tracing(
    debug: bool,
    config_log_level: Option<&str>,
    log_prefix: &str,
) -> Result<()> {
    let default_level = if debug {
        "debug"
    } else {
        config_log_level.unwrap_or("info")
    };
    let filter =
        EnvFilter::try_from_env("FABRO_LOG").unwrap_or_else(|_| EnvFilter::new(default_level));

    let log_dir = dirs::home_dir()
        .map(|h| h.join(".fabro").join("logs"))
        .unwrap_or_else(|| ".fabro/logs".into());

    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;

    let filename = chrono::Local::now()
        .format(&format!("{log_prefix}-%Y-%m-%d.log"))
        .to_string();
    let file_appender = rolling::never(&log_dir, &filename);

    let run_log_writer = run_log::init();

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(file_appender)
                .with_target(true)
                .with_ansi(false),
        )
        .with(
            fmt::layer()
                .with_writer(run_log_writer)
                .with_target(true)
                .with_ansi(false),
        )
        .init();

    Ok(())
}
