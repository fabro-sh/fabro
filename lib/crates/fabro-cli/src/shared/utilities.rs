#![expect(
    clippy::disallowed_types,
    reason = "sync CLI utilities: blocking std::io::Write is the intended output mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI utilities: blocking std::io::stdout is the intended output mechanism"
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cli_table::Color;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_validate::{Diagnostic, Severity};
use serde::Serialize;

pub(crate) fn read_workflow_file(path: &Path) -> anyhow::Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", path.display()))
}

pub(crate) fn print_json_pretty<T>(value: &T) -> anyhow::Result<()>
where
    T: Serialize + ?Sized,
{
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer_pretty(&mut handle, value)?;
    writeln!(handle)?;
    Ok(())
}

pub(crate) fn print_diagnostics(diagnostics: &[Diagnostic], styles: &Styles, printer: Printer) {
    for d in diagnostics {
        let location = match (&d.node_id, &d.edge) {
            (Some(node), _) => format!(" [node: {node}]"),
            (_, Some((from, to))) => format!(" [edge: {from} -> {to}]"),
            _ => String::new(),
        };
        match d.severity {
            Severity::Error => fabro_util::printerr!(
                printer,
                "{}{location}: {} ({})",
                styles.red.apply_to("error"),
                d.message,
                styles.dim.apply_to(&d.rule),
            ),
            Severity::Warning => fabro_util::printerr!(
                printer,
                "{}{location}: {} ({})",
                styles.yellow.apply_to("warning"),
                d.message,
                styles.dim.apply_to(&d.rule),
            ),
            Severity::Info => fabro_util::printerr!(
                printer,
                "{}",
                styles
                    .dim
                    .apply_to(format!("info{location}: {} ({})", d.message, d.rule)),
            ),
        }
    }
}

pub(crate) fn relative_path(path: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = path.strip_prefix(&cwd) {
            return rel.display().to_string();
        }
    }
    tilde_path(path)
}

pub(crate) fn format_tokens_human(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

pub(crate) fn format_usd_micros(usd_micros: i64) -> String {
    format!("${:.2}", usd_micros as f64 / 1_000_000.0)
}

pub(crate) fn tilde_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(suffix) = path.strip_prefix(&home) {
            return format!("~/{}", suffix.display());
        }
    }
    path.display().to_string()
}

pub(crate) fn absolute_or_current(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    }
}

pub(crate) fn color_if(use_color: bool, color: Color) -> Option<Color> {
    if use_color { Some(color) } else { None }
}

pub(crate) fn split_run_path(s: &str) -> Option<(&str, &str)> {
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return None;
    }
    s.split_once(':')
}

pub(crate) fn format_duration_ms(ms: u64) -> String {
    let duration = Duration::from_millis(ms);
    let secs = duration.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if duration.as_millis() >= 1000 {
        format!("{secs}s")
    } else {
        format!("{}ms", duration.as_millis())
    }
}

pub(crate) fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::{format_tokens_human, format_usd_micros};

    #[test]
    fn format_tokens_human_zero() {
        assert_eq!(format_tokens_human(0), "0");
    }

    #[test]
    fn format_tokens_human_small() {
        assert_eq!(format_tokens_human(999), "999");
    }

    #[test]
    fn format_tokens_human_thousands() {
        assert_eq!(format_tokens_human(1000), "1.0k");
    }

    #[test]
    fn format_tokens_human_mid_thousands() {
        assert_eq!(format_tokens_human(15234), "15.2k");
    }

    #[test]
    fn format_tokens_human_millions() {
        assert_eq!(format_tokens_human(1_000_000), "1.0m");
    }

    #[test]
    fn format_tokens_human_mid_millions() {
        assert_eq!(format_tokens_human(3_456_789), "3.5m");
    }

    #[test]
    fn format_usd_micros_two_decimals() {
        assert_eq!(format_usd_micros(570_000), "$0.57");
    }
}
