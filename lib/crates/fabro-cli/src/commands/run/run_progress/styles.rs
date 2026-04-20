use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use fabro_util::terminal::Styles;
use indicatif::ProgressStyle;

macro_rules! cached_style {
    ($name:ident, $template:expr) => {
        pub(super) fn $name() -> ProgressStyle {
            static STYLE: OnceLock<ProgressStyle> = OnceLock::new();
            STYLE
                .get_or_init(|| ProgressStyle::with_template($template).expect("valid template"))
                .clone()
        }
    };
}

cached_style!(
    style_header_running,
    "    {spinner:.dim} {wide_msg} {elapsed:.dim}"
);
cached_style!(style_header_done, "    {wide_msg:.dim} {prefix:.dim}");
cached_style!(
    style_stage_running,
    "    {spinner:.cyan} {wide_msg} {elapsed:.dim}"
);
cached_style!(style_stage_done, "    {wide_msg} {prefix:.dim}");
cached_style!(
    style_tool_running,
    "      {spinner:.dim} {wide_msg} {elapsed:.dim}"
);
cached_style!(style_tool_done, "      {wide_msg} {prefix:.dim}");
cached_style!(style_subagent_info, "        {wide_msg}");
cached_style!(style_branch_done, "        {wide_msg} {prefix:.dim}");
cached_style!(style_static_dim, "    {wide_msg:.dim}");
cached_style!(style_sandbox_detail, "             {wide_msg:.dim}");
cached_style!(style_empty, " ");

pub(super) fn green_check(styles: &Styles) -> String {
    styles.green.apply_to("\u{2713}").to_string()
}

pub(super) fn red_cross(styles: &Styles) -> String {
    styles.red.apply_to("\u{2717}").to_string()
}

pub(super) fn warning_glyph(styles: &Styles) -> String {
    styles.yellow.apply_to("\u{26a0}").to_string()
}

pub(crate) fn format_duration_short(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if d.as_millis() >= 1000 {
        format!("{secs}s")
    } else {
        format!("{}ms", d.as_millis())
    }
}

pub(super) fn terminal_hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

pub(super) fn format_number(n: f64) -> String {
    if (n - n.round()).abs() < f64::EPSILON {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Whole-number display intentionally narrows to i64 for formatting."
        )]
        let i = n as i64;
        format!("{i}")
    } else {
        format!("{n:.1}")
    }
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    let single_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.len() > max {
        let mut truncated: String = single_line.chars().take(max - 3).collect();
        truncated.push_str("...");
        truncated
    } else {
        single_line
    }
}

pub(super) fn last_line_truncated(s: &str, max: usize) -> String {
    let line = s
        .trim()
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.len() > max {
        let mut truncated: String = line.chars().take(max - 3).collect();
        truncated.push_str("...");
        truncated
    } else {
        line.to_string()
    }
}

pub(super) fn shorten_path(path: &str, working_directory: Option<&str>) -> String {
    if let Some(wd) = working_directory {
        if let Ok(rel) = Path::new(path).strip_prefix(wd) {
            return rel.display().to_string();
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = Path::new(path).strip_prefix(&cwd) {
            return rel.display().to_string();
        }
    }
    path.to_string()
}
