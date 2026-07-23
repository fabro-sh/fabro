#![expect(
    clippy::disallowed_methods,
    reason = "CLI logging setup: sync directory scan during startup"
)]
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use console::{Style, strip_ansi_codes};
use fabro_static::EnvVars;
use fabro_util::run_log::BufferedFileAppender;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::rolling;
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::format::{DefaultFields, FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::fmt::{FmtContext, FormattedFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::otel;

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
            init_stdout_subscriber(filter, std::io::stdout);
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
            let per_run_appender = open_buffered_appender(per_run_log_path)?;
            init_worker_stdout_subscriber(filter, std::io::stdout, per_run_appender);
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct TtyLogFormat {
    ansi: bool,
}

impl TtyLogFormat {
    fn new(ansi: bool) -> Self {
        Self { ansi }
    }
}

#[derive(Debug, Default)]
struct TtySpanFields {
    inner: DefaultFields,
}

impl<'writer> FormatFields<'writer> for TtySpanFields {
    fn format_fields<R: RecordFields>(
        &self,
        writer: Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        self.inner.format_fields(writer, fields)
    }
}

impl<S, N> FormatEvent<S, N> for TtyLogFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        const MESSAGE_WIDTH: usize = 42;

        let metadata = event.metadata();
        let mut fields = EventFieldVisitor::default();
        event.record(&mut fields);
        if !fields.has_field("run_id") {
            if let Some(run_id) = run_span_id(ctx) {
                fields.prepend_field("run_id", run_id);
            }
        }

        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        write!(
            writer,
            "{}  {}  ",
            self.dim(timestamp),
            self.level(*metadata.level())
        )?;

        if should_show_target(*metadata.level()) {
            write!(writer, "{}  ", self.dim(metadata.target()))?;
        }

        let message = fields.message.as_deref().unwrap_or_default();
        if !message.is_empty() {
            write!(writer, "{message}")?;
        }

        let formatted_fields = fields.format_fields();
        if !formatted_fields.is_empty() {
            if !message.is_empty() {
                let padding = MESSAGE_WIDTH.saturating_sub(message.len()) + 2;
                writer.write_str(&" ".repeat(padding))?;
            }

            write!(writer, "{}", self.dim(formatted_fields))?;
        }

        writeln!(writer)
    }
}

impl TtyLogFormat {
    fn level(self, level: Level) -> String {
        let padded = format!("{level:<5}");
        let style = match level {
            Level::ERROR => Style::new().red().bold(),
            Level::WARN => Style::new().yellow(),
            Level::INFO => Style::new().green(),
            Level::DEBUG => Style::new().cyan().dim(),
            Level::TRACE => Style::new().dim(),
        }
        .force_styling(self.ansi);

        format!("{}", style.apply_to(padded))
    }

    fn dim(self, value: impl std::fmt::Display) -> String {
        format!(
            "{}",
            Style::new().dim().force_styling(self.ansi).apply_to(value)
        )
    }
}

fn run_span_id<S, N>(ctx: &FmtContext<'_, S, N>) -> Option<String>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    let mut run_id = None;
    let scope = ctx.event_scope()?;

    for span in scope.from_root() {
        if span.metadata().name() != "run" {
            continue;
        }

        let extensions = span.extensions();
        if let Some(fields) = extensions.get::<FormattedFields<N>>() {
            let fields = strip_ansi_codes(&fields.fields);
            if let Some(id) = formatted_field_value(&fields, "id") {
                run_id = Some(id);
            }
        }
    }

    run_id
}

fn formatted_field_value(formatted_fields: &str, field_name: &str) -> Option<String> {
    let prefix = format!("{field_name}=");
    formatted_fields
        .split_whitespace()
        .find_map(|field| field.strip_prefix(&prefix))
        .map(ToOwned::to_owned)
}

#[derive(Default)]
struct EventFieldVisitor {
    message: Option<String>,
    fields:  Vec<(String, String)>,
}

impl EventFieldVisitor {
    fn prepend_field(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.fields.insert(0, (name.into(), value.into()));
    }

    fn has_field(&self, name: &str) -> bool {
        self.fields.iter().any(|(field_name, _)| field_name == name)
    }

    fn record_value(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
            return;
        }

        self.fields.push((field.name().to_string(), value));
    }

    fn format_fields(&self) -> String {
        let mut output = String::new();
        for (index, (name, value)) in self.fields.iter().enumerate() {
            if index > 0 {
                output.push(' ');
            }
            let _ = write!(output, "{name}={value}");
        }
        output
    }
}

impl Visit for EventFieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_value(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.record_value(field, value.to_string());
        } else {
            self.record_value(field, format!("{value:?}"));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_value(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_value(field, value.to_string());
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.record_value(field, value.to_string());
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.record_value(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_value(field, value.to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.record_value(field, value.to_string());
    }
}

fn should_show_target(level: Level) -> bool {
    matches!(level, Level::DEBUG | Level::TRACE)
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
        // Additive OTLP export: no-op unless an OTLP endpoint env is set.
        .with(otel::otel_layer())
        .init();
}

fn init_stdout_subscriber<W>(filter: EnvFilter, stdout_writer: W)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    if !std::io::stdout().is_terminal() {
        init_subscriber(filter, stdout_writer);
        return;
    }

    let ansi = console::colors_enabled();
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .fmt_fields(TtySpanFields::default())
                .with_writer(stdout_writer)
                .with_ansi(ansi)
                .event_format(TtyLogFormat::new(ansi)),
        )
        // Additive OTLP export: no-op unless an OTLP endpoint env is set.
        .with(otel::otel_layer())
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
        // Additive OTLP export: no-op unless an OTLP endpoint env is set.
        .with(otel::otel_layer())
        .init();
}

fn init_worker_stdout_subscriber<ServerWriter, RunWriter>(
    filter: EnvFilter,
    server_writer: ServerWriter,
    run_writer: RunWriter,
) where
    ServerWriter: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
    RunWriter: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    if !std::io::stdout().is_terminal() {
        init_worker_subscriber(filter, server_writer, run_writer);
        return;
    }

    let ansi = console::colors_enabled();
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .fmt_fields(TtySpanFields::default())
                .with_writer(server_writer)
                .with_ansi(ansi)
                .event_format(TtyLogFormat::new(ansi)),
        )
        .with(
            fmt::layer()
                .with_writer(run_writer)
                .with_target(true)
                .with_ansi(false),
        )
        // Additive OTLP export: no-op unless an OTLP endpoint env is set.
        .with(otel::otel_layer())
        .init();
}

fn open_buffered_appender(path: &Path) -> Result<BufferedFileAppender> {
    BufferedFileAppender::open(path)
        .with_context(|| format!("Failed to open log file: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::{fmt as std_fmt, io};

    use tracing::subscriber;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt as tracing_fmt, registry};

    use super::{TtyLogFormat, TtySpanFields};

    #[test]
    fn tty_format_timestamp_includes_calendar_date() {
        let output = render_tty_event(false, || {
            tracing::info!("API server started");
        });

        assert!(
            output.starts_with_timestamp(),
            "expected date-bearing timestamp at start, got: {output:?}"
        );
    }

    #[test]
    fn tty_format_info_hides_target_and_preserves_message_and_fields() {
        let output = render_tty_event(false, || {
            tracing::info!(
                target: "fabro_server::server",
                bind = %"/tmp/fabro.sock",
                "API server started"
            );
        });

        assert!(output.contains("INFO"));
        assert!(output.contains("API server started"));
        assert!(output.contains("bind=/tmp/fabro.sock"));
        assert!(
            !output.contains("fabro_server::server"),
            "INFO output should hide the target, got: {output:?}"
        );
    }

    #[test]
    fn tty_format_debug_includes_target() {
        let output = render_tty_event(false, || {
            tracing::debug!(
                target: "fabro_server::server",
                run = %"run_abc123",
                "Spawning worker"
            );
        });

        assert!(output.contains("DEBUG"));
        assert!(output.contains("fabro_server::server"));
        assert!(output.contains("Spawning worker"));
        assert!(output.contains("run=run_abc123"));
    }

    #[test]
    fn tty_format_includes_run_id_from_run_span() {
        let output = render_tty_event(false, || {
            let span = tracing::info_span!("run", id = %"01HV6D7S5YF4Z4B2M7K4N0Q6T9");
            let _guard = span.enter();
            let name = "fabro-v8";
            tracing::info!(name, duration_ms = 2610_u64, "Snapshot ready");
        });

        assert!(output.contains("Snapshot ready"));
        assert!(output.contains("run_id=01HV6D7S5YF4Z4B2M7K4N0Q6T9"));
        assert!(output.contains("name=\"fabro-v8\""));
        assert!(output.contains("duration_ms=2610"));
    }

    #[test]
    fn tty_format_with_color_includes_run_id_from_run_span() {
        let output = render_tty_event(true, || {
            let span = tracing::info_span!("run", id = %"01HV6D7S5YF4Z4B2M7K4N0Q6T9");
            let _guard = span.enter();
            tracing::info!("Snapshot ready");
        });

        assert!(output.contains("Snapshot ready"));
        assert!(
            output.contains("run_id=01HV6D7S5YF4Z4B2M7K4N0Q6T9"),
            "expected run_id from colored run span fields, got: {output:?}"
        );
    }

    #[test]
    fn tty_format_does_not_duplicate_event_run_id() {
        let output = render_tty_event(false, || {
            let span = tracing::info_span!("run", id = %"01HV6D7S5YF4Z4B2M7K4N0Q6T9");
            let _guard = span.enter();
            tracing::info!(run_id = %"event-run", "Run-specific event");
        });

        assert_eq!(output.matches("run_id=").count(), 1);
        assert!(output.contains("run_id=event-run"));
    }

    #[test]
    fn plain_format_includes_compact_run_span_id() {
        let output = render_plain_event(|| {
            let span = tracing::info_span!("run", id = %"01HV6D7S5YF4Z4B2M7K4N0Q6T9");
            let _guard = span.enter();
            tracing::info!("Creating Daytona sandbox");
        });

        assert!(output.contains("run{id=01HV6D7S5YF4Z4B2M7K4N0Q6T9}:"));
        assert!(output.contains("Creating Daytona sandbox"));
    }

    #[test]
    fn worker_run_log_stays_plain_when_stdout_uses_color() {
        let (_stdout, run_log) = render_worker_stdout_and_run_log_event(|| {
            let span = tracing::info_span!("run", id = %"01HV6D7S5YF4Z4B2M7K4N0Q6T9");
            let _guard = span.enter();
            tracing::info!("Pushed git ref to origin");
        });

        assert!(run_log.contains("Pushed git ref to origin"));
        assert!(
            !run_log.contains("\x1b["),
            "per-run disk log should not contain ANSI escapes, got: {run_log:?}"
        );
        assert!(run_log.contains("run{id=01HV6D7S5YF4Z4B2M7K4N0Q6T9}:"));
    }

    #[test]
    fn tty_format_with_color_contains_ansi_sequences() {
        let output = render_tty_event(true, || {
            tracing::warn!(attempt = 2, "LLM request failed, retrying");
        });

        assert!(
            output.contains("\x1b["),
            "color-enabled output should contain ANSI sequences, got: {output:?}"
        );
    }

    #[test]
    fn tty_format_without_color_contains_no_ansi_sequences() {
        let output = render_tty_event(false, || {
            tracing::error!(error = %"rate limited", "Request failed");
        });

        assert!(
            !output.contains("\x1b["),
            "color-disabled output should be plain, got: {output:?}"
        );
    }

    fn render_tty_event(ansi: bool, emit: impl FnOnce()) -> String {
        let output = CapturedTrace::default();
        let subscriber = registry().with(
            tracing_fmt::layer()
                .with_writer(output.clone())
                .fmt_fields(TtySpanFields::default())
                .with_ansi(ansi)
                .event_format(TtyLogFormat::new(ansi)),
        );

        subscriber::with_default(subscriber, emit);

        output.captured_output()
    }

    fn render_worker_stdout_and_run_log_event(emit: impl FnOnce()) -> (String, String) {
        let stdout = CapturedTrace::default();
        let run_log = CapturedTrace::default();
        let subscriber = registry()
            .with(
                tracing_fmt::layer()
                    .fmt_fields(TtySpanFields::default())
                    .with_writer(stdout.clone())
                    .with_ansi(true)
                    .event_format(TtyLogFormat::new(true)),
            )
            .with(
                tracing_fmt::layer()
                    .with_writer(run_log.clone())
                    .with_target(true)
                    .with_ansi(false),
            );

        subscriber::with_default(subscriber, emit);

        (stdout.captured_output(), run_log.captured_output())
    }

    fn render_plain_event(emit: impl FnOnce()) -> String {
        let output = CapturedTrace::default();
        let subscriber = registry().with(
            tracing_fmt::layer()
                .with_writer(output.clone())
                .with_target(true)
                .with_ansi(false),
        );

        subscriber::with_default(subscriber, emit);

        output.captured_output()
    }

    trait TimestampAssertion {
        fn starts_with_timestamp(&self) -> bool;
    }

    impl TimestampAssertion for str {
        fn starts_with_timestamp(&self) -> bool {
            let Some(timestamp) = self.get(..23) else {
                return false;
            };

            chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S%.3f").is_ok()
        }
    }

    #[derive(Clone, Default)]
    struct CapturedTrace {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl CapturedTrace {
        fn captured_output(&self) -> String {
            let buffer = self.buffer.lock().unwrap();
            String::from_utf8(buffer.clone()).unwrap()
        }
    }

    impl<'writer> MakeWriter<'writer> for CapturedTrace {
        type Writer = CapturedTraceWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            CapturedTraceWriter {
                buffer: Arc::clone(&self.buffer),
            }
        }
    }

    struct CapturedTraceWriter {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl io::Write for CapturedTraceWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.buffer.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl std_fmt::Debug for CapturedTraceWriter {
        fn fmt(&self, formatter: &mut std_fmt::Formatter<'_>) -> std_fmt::Result {
            formatter.debug_struct("CapturedTraceWriter").finish()
        }
    }
}
