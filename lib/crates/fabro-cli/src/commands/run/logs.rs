use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fabro_config::FabroSettingsExt;
use fabro_store::RunStore;
use fabro_util::terminal::Styles;
use fabro_workflows::run_lookup::{resolve_run_combined, runs_base};
use futures::StreamExt;
use tokio::time;
use tracing::{debug, info, warn};

use crate::args::{GlobalArgs, LogsArgs};
use crate::store;
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn run(args: &LogsArgs, styles: &Styles, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run).await?;

    info!(run_id = %run.run_id, "Showing logs");

    let since_cutoff = match &args.since {
        Some(value) => Some(parse_since(value)?),
        None => None,
    };

    let run_store = store::open_run_reader(&cli_settings.storage_dir(), &run.run_id).await?;
    let progress_path = run.path.join("progress.jsonl");
    let (all_lines, last_seq, use_store_follow) = if let Some(run_store) = run_store.as_ref() {
        match run_store.list_events().await {
            Ok(events) => {
                let last_seq = events.last().map_or(0, |event| event.seq);
                let lines = events
                    .iter()
                    .map(event_payload_line)
                    .collect::<Result<Vec<_>>>()?;
                (lines, last_seq, true)
            }
            Err(err) => {
                if !progress_path.exists() {
                    return Err(err).context("Failed to list store-backed run events");
                }
                warn!(
                    run_id = %run.run_id,
                    error = %err,
                    "Failed to read events from store; falling back to progress.jsonl"
                );
                (read_lines(&progress_path)?, 0, false)
            }
        }
    } else {
        if !progress_path.exists() {
            bail!("No progress.jsonl found for run '{}'", run.run_id);
        }
        (read_lines(&progress_path)?, 0, false)
    };
    let filtered = apply_filters(&all_lines, since_cutoff.as_ref(), args.tail);

    let stdout = io::stdout();
    let is_tty = stdout.is_terminal();
    let mut out = stdout.lock();

    for line in &filtered {
        if args.pretty {
            if let Some(formatted) = format_event_pretty(line, styles) {
                writeln!(out, "{formatted}")?;
            }
        } else {
            writeln!(out, "{line}")?;
        }
    }

    if args.follow {
        if use_store_follow {
            if let Some(run_store) = run_store.as_ref() {
                match follow_store_logs(
                    run_store.as_ref(),
                    if last_seq == 0 { 1 } else { last_seq + 1 },
                    args.pretty,
                    styles,
                    is_tty,
                )
                .await
                {
                    Ok(()) => {}
                    Err(err) => {
                        if !progress_path.exists() {
                            return Err(err);
                        }
                        warn!(
                            run_id = %run.run_id,
                            error = %err,
                            "Failed to follow store events; falling back to progress.jsonl"
                        );
                        let lines_seen = read_lines(&progress_path)?.len();
                        follow_logs(
                            &progress_path,
                            &run.path,
                            lines_seen,
                            args.pretty,
                            styles,
                            is_tty,
                        )?;
                    }
                }
            } else {
                unreachable!("store follow requested without a run store");
            }
        } else {
            follow_logs(
                &progress_path,
                &run.path,
                all_lines.len(),
                args.pretty,
                styles,
                is_tty,
            )?;
        }
    }

    Ok(())
}

fn read_lines(path: &Path) -> Result<Vec<String>> {
    let file = std::fs::File::open(path).context("Failed to open progress.jsonl")?;
    let reader = io::BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            lines.push(line);
        }
    }
    Ok(lines)
}

fn apply_filters(
    lines: &[String],
    since: Option<&DateTime<Utc>>,
    tail: Option<usize>,
) -> Vec<String> {
    let filtered: Vec<String> = match since {
        Some(cutoff) => lines
            .iter()
            .filter(|line| extract_timestamp(line).is_none_or(|ts| ts >= *cutoff))
            .cloned()
            .collect(),
        None => lines.to_vec(),
    };

    match tail {
        Some(n) if n < filtered.len() => filtered[filtered.len() - n..].to_vec(),
        _ => filtered,
    }
}

fn extract_timestamp(line: &str) -> Option<DateTime<Utc>> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let ts_str = value.get("ts")?.as_str()?;
    ts_str.parse::<DateTime<Utc>>().ok()
}

pub(crate) fn parse_since(s: &str) -> Result<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty --since value");
    }

    if let Some(duration) = try_parse_relative_duration(s) {
        return Ok(Utc::now() - duration);
    }

    if let Ok(ts) = s.parse::<DateTime<Utc>>() {
        return Ok(ts);
    }

    bail!(
        "invalid --since value '{s}' (expected relative like '42m', '2h', '7d' or ISO 8601 timestamp)"
    )
}

fn try_parse_relative_duration(s: &str) -> Option<chrono::Duration> {
    if s.len() < 2 {
        return None;
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: u64 = num_str.parse().ok()?;
    match unit {
        "s" => Some(chrono::Duration::seconds(i64::try_from(num).unwrap())),
        "m" => Some(chrono::Duration::minutes(i64::try_from(num).unwrap())),
        "h" => Some(chrono::Duration::hours(i64::try_from(num).unwrap())),
        "d" => Some(chrono::Duration::days(i64::try_from(num).unwrap())),
        _ => None,
    }
}

fn follow_logs(
    progress_path: &Path,
    run_dir: &Path,
    mut lines_seen: usize,
    pretty: bool,
    styles: &Styles,
    _is_tty: bool,
) -> Result<()> {
    let conclusion_path = run_dir.join("conclusion.json");
    let stdout = io::stdout();
    let mut out = stdout.lock();

    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));

        let all_lines = read_lines(progress_path)?;
        if all_lines.len() > lines_seen {
            for line in &all_lines[lines_seen..] {
                if pretty {
                    if let Some(formatted) = format_event_pretty(line, styles) {
                        writeln!(out, "{formatted}")?;
                    }
                } else {
                    writeln!(out, "{line}")?;
                }
            }
            out.flush()?;
            lines_seen = all_lines.len();
        }

        if conclusion_path.exists() && all_lines.len() <= lines_seen {
            debug!("Run concluded, stopping follow");
            break;
        }
    }

    Ok(())
}

async fn follow_store_logs(
    run_store: &dyn RunStore,
    seq: u32,
    pretty: bool,
    styles: &Styles,
    _is_tty: bool,
) -> Result<()> {
    let mut stream = run_store
        .watch_events_from(seq)
        .await
        .context("Failed to watch store-backed run events")?;
    let stdout = io::stdout();
    let mut out = stdout.lock();

    loop {
        match time::timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(Ok(event))) => {
                let line = event_payload_line(&event)?;
                if pretty {
                    if let Some(formatted) = format_event_pretty(&line, styles) {
                        writeln!(out, "{formatted}")?;
                    }
                } else {
                    writeln!(out, "{line}")?;
                }
                out.flush()?;
            }
            Ok(Some(Err(err))) => return Err(err.into()),
            Ok(None) => break,
            Err(_) => {
                if run_store
                    .get_conclusion()
                    .await
                    .context("Failed to read conclusion from store while following logs")?
                    .is_some()
                {
                    debug!("Run concluded, stopping follow");
                    break;
                }
                if run_store
                    .get_status()
                    .await
                    .context("Failed to read status from store while following logs")?
                    .is_some_and(|record| record.status.is_terminal())
                {
                    debug!("Run reached terminal status, stopping follow");
                    break;
                }
            }
        }
    }

    Ok(())
}

fn event_payload_line(event: &fabro_store::EventEnvelope) -> Result<String> {
    serde_json::to_string(event.payload.as_value()).map_err(Into::into)
}

fn render_indented_markdown(styles: &Styles, text: &str, indent: &str) -> String {
    let term_width = Styles::terminal_width();
    let wrap_width = term_width.saturating_sub(indent.len());
    let rendered = styles.render_markdown_width(text, wrap_width);
    rendered
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn format_event_pretty(line: &str, styles: &Styles) -> Option<String> {
    let envelope: serde_json::Value = serde_json::from_str(line).ok()?;
    let event = envelope.get("event")?.as_str()?;
    let ts = format_timestamp(envelope.get("ts")?.as_str()?);

    match event {
        "WorkflowRunStarted" => {
            let name = str_field(&envelope, "workflow_name").unwrap_or("?");
            let run_id = str_field(&envelope, "run_id").unwrap_or("?");
            let header = format!(
                "{} {} {}  {}",
                styles.dim.apply_to(&ts),
                styles.bold_cyan.apply_to("\u{25b6}"),
                styles.bold.apply_to(name),
                styles.dim.apply_to(run_id),
            );
            match str_field(&envelope, "goal") {
                Some(goal) if !goal.is_empty() => {
                    let body = render_indented_markdown(styles, goal, "            ");
                    Some(format!("{header}\n{body}\n"))
                }
                _ => Some(header),
            }
        }
        "WorkflowRunCompleted" => {
            let duration = format_duration_ms(envelope.get("duration_ms"));
            let status_str = match str_field(&envelope, "status") {
                Some(status) if !status.is_empty() => status,
                _ => "success",
            };
            let status_upper = status_str.to_uppercase();
            let status_style = match status_str {
                "success" | "partial_success" => &styles.bold_green,
                _ => &styles.bold_red,
            };
            let cost = format_cost(envelope.get("total_cost"));

            let mut lines = vec![format!(
                "{} {} {}  {}",
                styles.dim.apply_to(&ts),
                status_style.apply_to(format!("\u{2713} {status_upper}")),
                styles.bold.apply_to(&duration),
                styles.dim.apply_to(&cost),
            )];

            if let Some(usage) = envelope.get("usage") {
                let total = usage
                    .get("total_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                let pad = " ".repeat(ts.len() + 1);
                if total > 0 {
                    lines.push(format!(
                        "{}{}",
                        pad,
                        styles.dim.apply_to(format!(
                            "Tokens: {}",
                            format_tokens(u64::try_from(total).unwrap())
                        ))
                    ));
                }
                if let Some(cache_read) = usage
                    .get("cache_read_tokens")
                    .and_then(serde_json::Value::as_i64)
                {
                    let cache_write = usage
                        .get("cache_write_tokens")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0);
                    lines.push(format!(
                        "{}{}",
                        pad,
                        styles.dim.apply_to(format!(
                            "Cache:  {} read, {} write",
                            format_tokens(u64::try_from(cache_read).unwrap()),
                            format_tokens(u64::try_from(cache_write).unwrap())
                        ))
                    ));
                }
                if let Some(reasoning) = usage
                    .get("reasoning_tokens")
                    .and_then(serde_json::Value::as_i64)
                {
                    if reasoning > 0 {
                        lines.push(format!(
                            "{}{}",
                            pad,
                            styles.dim.apply_to(format!(
                                "Reasoning: {} tokens",
                                format_tokens(u64::try_from(reasoning).unwrap())
                            ))
                        ));
                    }
                }
            }

            Some(lines.join("\n"))
        }
        "WorkflowRunFailed" => {
            let error = str_field(&envelope, "error").unwrap_or("unknown error");
            Some(format!(
                "{} {} {}",
                styles.dim.apply_to(&ts),
                styles.bold_red.apply_to("\u{2717} Failed"),
                styles.red.apply_to(error),
            ))
        }
        "RunNotice" => {
            let level = str_field(&envelope, "level").unwrap_or("info");
            let code = str_field(&envelope, "code").unwrap_or("");
            let message = str_field(&envelope, "message").unwrap_or("");
            let label = match level {
                "warn" => styles.yellow.apply_to("Warning:").to_string(),
                "error" => styles.bold_red.apply_to("Error:").to_string(),
                _ => styles.bold.apply_to("Info:").to_string(),
            };
            let code_suffix = if code.is_empty() {
                String::new()
            } else {
                format!(" {}", styles.dim.apply_to(format!("[{code}]")))
            };
            Some(format!(
                "{} {} {}{}",
                styles.dim.apply_to(&ts),
                label,
                message,
                code_suffix,
            ))
        }
        "StageStarted" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            Some(format!(
                "{} {} {}",
                styles.dim.apply_to(&ts),
                styles.bold_cyan.apply_to("\u{25b6}"),
                styles.bold.apply_to(label),
            ))
        }
        "StageCompleted" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            let duration = format_duration_ms(envelope.get("duration_ms"));
            let cost = format_cost(envelope.get("cost"));
            let turns = envelope
                .get("turns")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let tools = envelope
                .get("tool_calls")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let tokens = envelope
                .get("total_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let stats = format!("({turns} turns, {tools} tools, {})", format_tokens(tokens));
            Some(format!(
                "{} {} {}  {}  {}  {}",
                styles.dim.apply_to(&ts),
                styles.green.apply_to("\u{2713}"),
                styles.bold.apply_to(label),
                cost,
                duration,
                styles.dim.apply_to(&stats),
            ))
        }
        "StageFailed" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            let error = str_field(&envelope, "error").unwrap_or("unknown error");
            Some(format!(
                "{} {} {}  {}",
                styles.dim.apply_to(&ts),
                styles.red.apply_to("\u{2717}"),
                styles.bold.apply_to(label),
                styles.red.apply_to(error),
            ))
        }
        "Agent.AssistantMessage" => {
            let stage = str_field(&envelope, "node_id").unwrap_or("?");
            let model = str_field(&envelope, "model").unwrap_or("?");
            let text = str_field(&envelope, "text").unwrap_or("");
            let header = format!(
                "{} {} {} {}{}{}",
                styles.dim.apply_to(&ts),
                "\u{1f4ac}",
                styles.bold.apply_to(stage),
                styles.dim.apply_to("["),
                styles.dim.apply_to(model),
                styles.dim.apply_to("]"),
            );
            let body = render_indented_markdown(styles, text, "            ");
            Some(format!("{header}\n{body}\n"))
        }
        "Agent.ToolCallStarted" => {
            let tool = str_field(&envelope, "tool_name").unwrap_or("?");
            let detail = tool_detail(&envelope);
            let display = match detail {
                Some(value) => format!("{tool}({value})"),
                None => tool.to_string(),
            };
            Some(format!(
                "{}    {} {}",
                styles.dim.apply_to(&ts),
                styles.dim.apply_to("\u{2699}"),
                styles.dim.apply_to(&display),
            ))
        }
        "Agent.ToolCallCompleted" => {
            let tool = str_field(&envelope, "tool_name").unwrap_or("?");
            let is_error = envelope
                .get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let detail = tool_detail(&envelope);
            let display = match detail {
                Some(value) => format!("{tool}({value})"),
                None => tool.to_string(),
            };
            let glyph = if is_error { "\u{2717}" } else { "\u{2713}" };
            let style = if is_error { &styles.red } else { &styles.green };
            Some(format!(
                "{}    {} {}",
                styles.dim.apply_to(&ts),
                style.apply_to(glyph),
                display,
            ))
        }
        "EdgeSelected" => {
            let to = str_field(&envelope, "to_node_id").unwrap_or("?");
            let reason = str_field(&envelope, "reason").unwrap_or("?");
            let condition = str_field(&envelope, "condition");
            let detail = match condition {
                Some(value) => format!("  [{value}]"),
                None => String::new(),
            };
            Some(format!(
                "{}    {} {} {}{}",
                styles.dim.apply_to(&ts),
                styles.dim.apply_to("\u{2192}"),
                to,
                styles.dim.apply_to(reason),
                styles.dim.apply_to(&detail),
            ))
        }
        "Sandbox.Ready" => {
            let provider = str_field(&envelope, "sandbox_provider").unwrap_or("?");
            let duration = format_duration_ms(envelope.get("duration_ms"));
            Some(format!(
                "{}   Sandbox: {}  {}",
                styles.dim.apply_to(&ts),
                provider,
                styles.dim.apply_to(&duration),
            ))
        }
        "SetupCompleted" => {
            let count = envelope
                .get("command_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let duration = format_duration_ms(envelope.get("duration_ms"));
            Some(format!(
                "{}   Setup: {} commands  {}",
                styles.dim.apply_to(&ts),
                count,
                styles.dim.apply_to(&duration),
            ))
        }
        "Agent.CompactionCompleted" => {
            let original = envelope
                .get("original_turn_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let preserved = envelope
                .get("preserved_turn_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            Some(format!(
                "{}   {}",
                styles.dim.apply_to(&ts),
                styles
                    .dim
                    .apply_to(format!("compaction: {original}\u{2192}{preserved} turns")),
            ))
        }
        "ParallelStarted" => {
            let count = envelope
                .get("branch_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            Some(format!(
                "{} {} Parallel  {} branches",
                styles.dim.apply_to(&ts),
                styles.bold_cyan.apply_to("\u{25b6}"),
                count,
            ))
        }
        "ParallelBranchStarted" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            Some(format!(
                "{}     {} {}",
                styles.dim.apply_to(&ts),
                styles.cyan.apply_to("\u{25b6}"),
                label,
            ))
        }
        "ParallelBranchCompleted" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            Some(format!(
                "{}     {} {}",
                styles.dim.apply_to(&ts),
                styles.green.apply_to("\u{2713}"),
                label,
            ))
        }
        "ParallelCompleted" => {
            let duration = format_duration_ms(envelope.get("duration_ms"));
            Some(format!(
                "{} {} Parallel  {}",
                styles.dim.apply_to(&ts),
                styles.green.apply_to("\u{2713}"),
                duration,
            ))
        }
        "PullRequestCreated" => {
            let url = str_field(&envelope, "pr_url").unwrap_or("?");
            let draft = envelope
                .get("draft")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let label = if draft { "Draft PR:" } else { "PR:" };
            Some(format!(
                "{} {} {}",
                styles.dim.apply_to(&ts),
                styles.bold.apply_to(label),
                url,
            ))
        }
        "PullRequestFailed" => {
            let error = str_field(&envelope, "error").unwrap_or("unknown error");
            Some(format!(
                "{} {} {}",
                styles.dim.apply_to(&ts),
                styles.bold_red.apply_to("PR failed:"),
                styles.red.apply_to(error),
            ))
        }
        "RetroCompleted" => {
            let duration = format_duration_ms(envelope.get("duration_ms"));
            Some(format!(
                "{} {} Retro  {}",
                styles.dim.apply_to(&ts),
                styles.green.apply_to("\u{2713}"),
                duration,
            ))
        }
        "RetroFailed" => {
            let error = str_field(&envelope, "error").unwrap_or("unknown error");
            let duration = format_duration_ms(envelope.get("duration_ms"));
            Some(format!(
                "{} {} Retro  {}  {}",
                styles.dim.apply_to(&ts),
                styles.bold_red.apply_to("\u{2717}"),
                duration,
                styles.red.apply_to(error),
            ))
        }
        "RetroStarted" => Some(format!(
            "{} {} Retro",
            styles.dim.apply_to(&ts),
            styles.bold_cyan.apply_to("\u{25b6}"),
        )),
        _ => None,
    }
}

fn str_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn format_timestamp(ts: &str) -> String {
    ts.parse::<DateTime<Utc>>()
        .map_or_else(|_| ts.to_string(), |dt| dt.format("%H:%M:%S").to_string())
}

fn format_duration_ms(value: Option<&serde_json::Value>) -> String {
    let ms = value.and_then(serde_json::Value::as_u64).unwrap_or(0);
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        let secs = ms as f64 / 1000.0;
        if secs < 60.0 {
            format!("{secs:.0}s")
        } else {
            let mins = secs / 60.0;
            format!("{mins:.1}m")
        }
    }
}

fn format_cost(value: Option<&serde_json::Value>) -> String {
    let cost = value.and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    if cost > 0.0 {
        format!("${cost:.2}")
    } else {
        String::new()
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{:.1}k toks", tokens as f64 / 1000.0)
    } else {
        format!("{tokens} toks")
    }
}

fn tool_detail(envelope: &serde_json::Value) -> Option<String> {
    let tool_name = str_field(envelope, "tool_name")?;
    let arguments = envelope.get("arguments")?;
    let arg = |key: &str| arguments.get(key).and_then(|v| v.as_str());

    match tool_name {
        "bash" | "shell" | "execute_command" => arg("command").map(|c| truncate(c, 60)),
        "glob" => arg("pattern").map(String::from),
        "grep" | "ripgrep" => arg("pattern").map(|p| truncate(p, 40)),
        "read_file" | "read" => arg("path")
            .or_else(|| arg("file_path"))
            .map(|p| truncate(p, 60)),
        "write_file" | "write" | "create_file" => arg("path")
            .or_else(|| arg("file_path"))
            .map(|p| truncate(p, 60)),
        "edit_file" | "edit" => arg("path")
            .or_else(|| arg("file_path"))
            .map(|p| truncate(p, 60)),
        "list_dir" => arg("path")
            .or_else(|| arg("file_path"))
            .map(|p| truncate(p, 60)),
        "web_search" => arg("query").map(|q| truncate(q, 60)),
        "web_fetch" => arg("url").map(|u| truncate(u, 60)),
        "spawn_agent" => arg("task").map(|t| truncate(t, 60)),
        "wait" | "send_input" | "close_agent" => arg("agent_id").map(String::from),
        "use_skill" => arg("skill_name").map(String::from),
        "apply_patch" => Some("…".into()),
        "read_many_files" => arguments
            .get("paths")
            .and_then(|v| v.as_array())
            .map(|a| format!("{} files", a.len())),
        _ => None,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let boundary = s.floor_char_boundary(max.saturating_sub(1));
        format!("{}\u{2026}", &s[..boundary])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_color_styles() -> Styles {
        Styles::new(false)
    }

    #[test]
    fn parse_since_relative_minutes() {
        let before = Utc::now();
        let result = parse_since("42m").unwrap();
        let after = Utc::now();
        let expected_lower = after - chrono::Duration::minutes(42) - chrono::Duration::seconds(1);
        let expected_upper = before - chrono::Duration::minutes(42) + chrono::Duration::seconds(1);
        assert!(result >= expected_lower && result <= expected_upper);
    }

    #[test]
    fn parse_since_relative_hours() {
        let before = Utc::now();
        let result = parse_since("2h").unwrap();
        let expected = before - chrono::Duration::hours(2);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn parse_since_relative_days() {
        let before = Utc::now();
        let result = parse_since("7d").unwrap();
        let expected = before - chrono::Duration::days(7);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn parse_since_iso8601() {
        let result = parse_since("2026-01-01T12:00:00Z").unwrap();
        assert_eq!(result.to_rfc3339(), "2026-01-01T12:00:00+00:00");
    }

    #[test]
    fn parse_since_invalid() {
        assert!(parse_since("").is_err());
        assert!(parse_since("abc").is_err());
        assert!(parse_since("notadate").is_err());
    }

    #[test]
    fn tail_returns_last_n_lines() {
        let lines: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        let result = apply_filters(&lines, None, Some(3));
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "line 7");
        assert_eq!(result[2], "line 9");
    }

    #[test]
    fn tail_all_when_n_exceeds_total() {
        let lines: Vec<String> = (0..3).map(|i| format!("line {i}")).collect();
        let result = apply_filters(&lines, None, Some(100));
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn since_filters_by_timestamp() {
        let cutoff = "2026-01-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let lines = vec![
            r#"{"ts":"2026-01-01T11:00:00Z","event":"StageStarted"}"#.to_string(),
            r#"{"ts":"2026-01-01T12:30:00Z","event":"StageCompleted"}"#.to_string(),
            r#"{"ts":"2026-01-01T13:00:00Z","event":"WorkflowRunCompleted"}"#.to_string(),
        ];
        let result = apply_filters(&lines, Some(&cutoff), None);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn raw_lines_pass_through_verbatim() {
        let lines = vec![
            r#"{"ts":"2026-01-01T12:00:00Z","event":"StageStarted","node_label":"plan"}"#
                .to_string(),
        ];
        let result = apply_filters(&lines, None, None);
        assert_eq!(result, lines);
    }

    #[test]
    fn pretty_stage_started() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:09Z","event":"StageStarted","node_label":"plan","node_id":"plan","stage_index":0}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("plan"), "got: {result}");
        assert!(result.contains("\u{25b6}"), "got: {result}");
    }

    #[test]
    fn pretty_stage_completed() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:15Z","event":"StageCompleted","node_label":"plan","cost":0.12,"duration_ms":8000,"turns":3,"tool_calls":2,"total_tokens":15200}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("plan"), "got: {result}");
        assert!(result.contains("$0.12"), "got: {result}");
        assert!(result.contains("8s"), "got: {result}");
        assert!(result.contains("3 turns"), "got: {result}");
    }

    #[test]
    fn pretty_assistant_message() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:12Z","event":"Agent.AssistantMessage","node_id":"plan","model":"claude-opus-4-6","text":"I'll start by reading the code.","usage":{"input_tokens":100,"output_tokens":50},"tool_call_count":0}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("plan"), "got: {result}");
        assert!(result.contains("claude-opus-4-6"), "got: {result}");
        assert!(result.contains("reading the code"), "got: {result}");
    }

    #[test]
    fn pretty_tool_call_started() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:12Z","event":"Agent.ToolCallStarted","tool_name":"read_file","tool_call_id":"tc_1","arguments":{"path":"src/main.rs"}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("read_file"), "got: {result}");
        assert!(result.contains("src/main.rs"), "got: {result}");
    }

    #[test]
    fn pretty_skips_noise_events() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:12Z","event":"Agent.TextDelta","delta":"hello"}"#;
        assert!(format_event_pretty(line, &styles).is_none());
    }

    #[test]
    fn pretty_skips_assistant_output_replace_noise_event() {
        let styles = no_color_styles();
        let line =
            r#"{"ts":"2026-01-01T14:23:12Z","event":"Agent.AssistantOutputReplace","text":""}"#;
        assert!(format_event_pretty(line, &styles).is_none());
    }

    #[test]
    fn pretty_unknown_events_return_none() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:12Z","event":"SomeFutureEvent","data":123}"#;
        assert!(format_event_pretty(line, &styles).is_none());
    }

    #[test]
    fn pretty_workflow_run_started() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:01Z","run_id":"abc123","event":"WorkflowRunStarted","workflow_name":"smoke"}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("smoke"), "got: {result}");
        assert!(result.contains("abc123"), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_started_with_goal() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:01Z","run_id":"abc123","event":"WorkflowRunStarted","workflow_name":"smoke","goal":"Fix the bug"}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("smoke"), "got: {result}");
        assert!(result.contains("abc123"), "got: {result}");
        assert!(result.contains("Fix the bug"), "got: {result}");
        assert!(result.contains('\n'), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_started_without_goal_no_extra_lines() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:01Z","run_id":"abc123","event":"WorkflowRunStarted","workflow_name":"smoke"}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(!result.contains('\n'), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_completed() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:32Z","run_id":"abc123","event":"WorkflowRunCompleted","duration_ms":25000,"status":"success","total_cost":0.57,"usage":{"input_tokens":5000,"output_tokens":2000,"total_tokens":7000,"cache_read_tokens":3000,"cache_write_tokens":500,"reasoning_tokens":800}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("SUCCESS"), "got: {result}");
        assert!(result.contains("25s"), "got: {result}");
        assert!(result.contains("$0.57"), "got: {result}");
        assert!(result.contains("7.0k toks"), "got: {result}");
        assert!(result.contains("Cache:"), "got: {result}");
        assert!(result.contains("3.0k toks read"), "got: {result}");
        assert!(result.contains("Reasoning:"), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_completed_backward_compat() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:32Z","run_id":"abc123","event":"WorkflowRunCompleted","duration_ms":25000,"total_cost":0.57}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("SUCCESS"), "got: {result}");
        assert!(result.contains("25s"), "got: {result}");
        assert!(result.contains("$0.57"), "got: {result}");
        assert!(!result.contains("Tokens:"), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_completed_fail_status() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:32Z","event":"WorkflowRunCompleted","duration_ms":25000,"status":"fail"}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("FAIL"), "got: {result}");
    }

    #[test]
    fn pretty_pull_request_created() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"PullRequestCreated","pr_url":"https://github.com/owner/repo/pull/42","pr_number":42,"draft":false}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("PR:"), "got: {result}");
        assert!(
            result.contains("https://github.com/owner/repo/pull/42"),
            "got: {result}"
        );
    }

    #[test]
    fn pretty_pull_request_created_draft() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"PullRequestCreated","pr_url":"https://github.com/owner/repo/pull/42","pr_number":42,"draft":true}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("Draft PR:"), "got: {result}");
    }

    #[test]
    fn pretty_pull_request_failed() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"PullRequestFailed","error":"auth token expired"}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("PR failed:"), "got: {result}");
        assert!(result.contains("auth token expired"), "got: {result}");
    }

    #[test]
    fn pretty_run_notice_warn() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"RunNotice","level":"warn","code":"sandbox_cleanup_failed","message":"sandbox cleanup failed: boom"}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("Warning:"), "got: {result}");
        assert!(
            result.contains("sandbox cleanup failed: boom"),
            "got: {result}"
        );
        assert!(result.contains("[sandbox_cleanup_failed]"), "got: {result}");
    }

    #[test]
    fn pretty_run_notice_error() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"RunNotice","level":"error","code":"launch_failed","message":"failed to start engine"}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("Error:"), "got: {result}");
        assert!(result.contains("failed to start engine"), "got: {result}");
        assert!(result.contains("[launch_failed]"), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_failed() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:32Z","run_id":"abc123","event":"WorkflowRunFailed","error":"sandbox timeout"}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("Failed"), "got: {result}");
        assert!(result.contains("sandbox timeout"), "got: {result}");
    }

    #[test]
    fn format_duration_ms_subsecond() {
        assert_eq!(format_duration_ms(Some(&serde_json::json!(500))), "500ms");
    }

    #[test]
    fn format_duration_ms_seconds() {
        assert_eq!(format_duration_ms(Some(&serde_json::json!(8000))), "8s");
    }

    #[test]
    fn format_duration_ms_minutes() {
        assert_eq!(format_duration_ms(Some(&serde_json::json!(90000))), "1.5m");
    }

    #[test]
    fn format_tokens_small() {
        assert_eq!(format_tokens(500), "500 toks");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(15200), "15.2k toks");
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let result = truncate("a very long command string here", 15);
        assert!(result.chars().count() <= 15, "got: {result}");
        assert!(result.ends_with('\u{2026}'));
    }
}
