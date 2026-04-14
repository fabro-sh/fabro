use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::json::normalize_json_value;
use fabro_util::printer::Printer;
use fabro_util::redact::redact_jsonl_line;
use fabro_util::terminal::Styles;
use tokio::time;
use tracing::{debug, info};

use crate::args::LogsArgs;
use crate::command_context::CommandContext;
use crate::server_client;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::format_usd_micros;

const FOLLOW_TERMINAL_GRACE: Duration = Duration::from_millis(500);

pub(crate) async fn run(
    args: &LogsArgs,
    styles: &Styles,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_target(&args.server, printer, cli.clone(), cli_layer)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    let run = lookup.resolve(&args.run)?;
    let client = lookup.client();

    let run_id = run.run_id();
    info!(run_id = %run_id, "Showing logs");

    let since_cutoff = match &args.since {
        Some(value) => Some(parse_since(value)?),
        None => None,
    };

    let events = client
        .list_run_events(&run_id, None, None)
        .await
        .context("Failed to list server-backed run events")?;
    let last_seq = events.last().map_or(0, |event| event.seq);
    let all_lines = events
        .iter()
        .map(event_payload_line)
        .collect::<Result<Vec<_>>>()?;
    let filtered = apply_filters(&all_lines, since_cutoff.as_ref(), args.tail);

    let stdout = io::stdout();
    let is_tty = stdout.is_terminal();
    let mut out = stdout.lock();
    let pretty = args.pretty && cli.output.format != OutputFormat::Json;

    for line in &filtered {
        if pretty {
            if let Some(formatted) = format_event_pretty(line, styles) {
                writeln!(out, "{formatted}")?;
            }
        } else {
            writeln!(out, "{line}")?;
        }
    }

    if args.follow {
        follow_store_logs(
            client,
            &run_id,
            if last_seq == 0 { 1 } else { last_seq + 1 },
            pretty,
            styles,
            is_tty,
        )
        .await?;
    }

    Ok(())
}

fn event_name(event: &fabro_store::EventEnvelope) -> Option<&str> {
    event.payload.as_value().get("event")?.as_str()
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

async fn follow_store_logs(
    client: &server_client::ServerStoreClient,
    run_id: &fabro_types::RunId,
    seq: u32,
    pretty: bool,
    styles: &Styles,
    _is_tty: bool,
) -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut next_seq = seq;
    let mut terminal_deadline = None;

    loop {
        match time::timeout(
            Duration::from_millis(200),
            client.list_run_events(run_id, Some(next_seq), None),
        )
        .await
        {
            Ok(Ok(events)) => {
                let had_events = !events.is_empty();
                let saw_terminal = events
                    .iter()
                    .any(|event| matches!(event_name(event), Some("run.completed" | "run.failed")));
                for event in events {
                    let line = event_payload_line(&event)?;
                    if pretty {
                        if let Some(formatted) = format_event_pretty(&line, styles) {
                            writeln!(out, "{formatted}")?;
                        }
                    } else {
                        writeln!(out, "{line}")?;
                    }
                    out.flush()?;
                    next_seq = event.seq.saturating_add(1);
                }
                if saw_terminal || (terminal_deadline.is_some() && had_events) {
                    terminal_deadline = Some(time::Instant::now() + FOLLOW_TERMINAL_GRACE);
                }
            }
            Err(_) => {
                if run_concluded(client, run_id).await? {
                    terminal_deadline
                        .get_or_insert_with(|| time::Instant::now() + FOLLOW_TERMINAL_GRACE);
                }
            }
            Ok(Err(err)) => return Err(err),
        }

        let Some(deadline) = terminal_deadline else {
            continue;
        };
        if time::Instant::now() < deadline {
            continue;
        }

        let flushed_next_seq =
            flush_remaining_store_events(client, run_id, next_seq, pretty, styles, &mut out)
                .await?;
        if flushed_next_seq > next_seq {
            next_seq = flushed_next_seq;
            terminal_deadline = Some(time::Instant::now() + FOLLOW_TERMINAL_GRACE);
            continue;
        }

        debug!("Run reached terminal status and log tail is quiet, stopping follow");
        break;
    }

    Ok(())
}

async fn run_concluded(
    client: &server_client::ServerStoreClient,
    run_id: &fabro_types::RunId,
) -> Result<bool> {
    let state = client
        .get_run_state(run_id)
        .await
        .context("Failed to read run state from server while following logs")?;
    Ok(state.conclusion.is_some()
        || state
            .status
            .is_some_and(|record| record.status.is_terminal()))
}

async fn flush_remaining_store_events(
    client: &server_client::ServerStoreClient,
    run_id: &fabro_types::RunId,
    next_seq: u32,
    pretty: bool,
    styles: &Styles,
    out: &mut dyn Write,
) -> Result<u32> {
    let events = client
        .list_run_events(run_id, Some(next_seq), None)
        .await
        .context("Failed to list server-backed run events while finalizing follow")?;

    let mut next_seq = next_seq;
    for event in events {
        let line = event_payload_line(&event)?;
        if pretty {
            if let Some(formatted) = format_event_pretty(&line, styles) {
                writeln!(out, "{formatted}")?;
            }
        } else {
            writeln!(out, "{line}")?;
        }
        next_seq = event.seq.saturating_add(1);
    }
    out.flush()?;
    Ok(next_seq)
}

fn event_payload_line(event: &fabro_store::EventEnvelope) -> Result<String> {
    let mut value = normalize_json_value(event.payload.as_value().clone());
    restore_empty_run_properties(&mut value);
    let line = serde_json::to_string(&value)?;
    Ok(redact_jsonl_line(&line))
}

fn restore_empty_run_properties(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let Some(event_name) = object.get("event").and_then(serde_json::Value::as_str) else {
        return;
    };
    if matches!(event_name, "run.submitted" | "run.running") && !object.contains_key("properties") {
        let run_id = object.remove("run_id");
        let ts = object.remove("ts");
        object.insert("properties".to_string(), serde_json::json!({}));
        if let Some(run_id) = run_id {
            object.insert("run_id".to_string(), run_id);
        }
        if let Some(ts) = ts {
            object.insert("ts".to_string(), ts);
        }
    }
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
        "run.started" => {
            let name = prop_str_field(&envelope, "name").unwrap_or("?");
            let run_id = str_field(&envelope, "run_id").unwrap_or("?");
            let header = format!(
                "{} {} {}  {}",
                styles.dim.apply_to(&ts),
                styles.bold_cyan.apply_to("\u{25b6}"),
                styles.bold.apply_to(name),
                styles.dim.apply_to(run_id),
            );
            match prop_str_field(&envelope, "goal") {
                Some(goal) if !goal.is_empty() => {
                    let body = render_indented_markdown(styles, goal, "            ");
                    Some(format!("{header}\n{body}\n"))
                }
                _ => Some(header),
            }
        }
        "run.completed" => {
            let duration = format_duration_ms(prop_field(&envelope, "duration_ms"));
            let status_str = match prop_str_field(&envelope, "status") {
                Some(status) if !status.is_empty() => status,
                _ => "success",
            };
            let status_upper = status_str.to_uppercase();
            let status_style = match status_str {
                "success" | "partial_success" => &styles.bold_green,
                _ => &styles.bold_red,
            };
            let cost = format_cost(
                prop_field(&envelope, "total_usd_micros")
                    .or_else(|| prop_field(&envelope, "total_cost")),
            );

            let mut lines = vec![format!(
                "{} {} {}  {}",
                styles.dim.apply_to(&ts),
                status_style.apply_to(format!("\u{2713} {status_upper}")),
                styles.bold.apply_to(&duration),
                styles.dim.apply_to(&cost),
            )];

            if let Some(billing) =
                prop_field(&envelope, "billing").or_else(|| prop_field(&envelope, "usage"))
            {
                let total = billing
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
                if let Some(cache_read) = billing
                    .get("cache_read_tokens")
                    .and_then(serde_json::Value::as_i64)
                {
                    let cache_write = billing
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
                if let Some(reasoning) = billing
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
        "run.failed" => {
            let error = prop_str_field(&envelope, "error").unwrap_or("unknown error");
            Some(format!(
                "{} {} {}",
                styles.dim.apply_to(&ts),
                styles.bold_red.apply_to("\u{2717} Failed"),
                styles.red.apply_to(error),
            ))
        }
        "run.notice" => {
            let level = prop_str_field(&envelope, "level").unwrap_or("info");
            let code = prop_str_field(&envelope, "code").unwrap_or("");
            let message = prop_str_field(&envelope, "message").unwrap_or("");
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
        "stage.started" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            Some(format!(
                "{} {} {}",
                styles.dim.apply_to(&ts),
                styles.bold_cyan.apply_to("\u{25b6}"),
                styles.bold.apply_to(label),
            ))
        }
        "stage.completed" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            let duration = format_duration_ms(prop_field(&envelope, "duration_ms"));
            let billing =
                prop_field(&envelope, "billing").or_else(|| prop_field(&envelope, "usage"));
            let cost = format_cost(
                billing
                    .and_then(|value| value.get("total_usd_micros"))
                    .or_else(|| billing.and_then(|value| value.get("cost"))),
            );
            let input_tokens = billing
                .and_then(|value| value.get("input_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let output_tokens = billing
                .and_then(|value| value.get("output_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let token_total = input_tokens.saturating_add(output_tokens);
            let mut line = format!(
                "{} {} {}  {}  {}",
                styles.dim.apply_to(&ts),
                styles.green.apply_to("\u{2713}"),
                styles.bold.apply_to(label),
                cost,
                duration,
            );
            if token_total > 0 {
                let _ = write!(
                    line,
                    "  {}",
                    styles.dim.apply_to(format_tokens(token_total))
                );
            }
            Some(line)
        }
        "stage.failed" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            let error = prop_str_field(&envelope, "error").unwrap_or("unknown error");
            Some(format!(
                "{} {} {}  {}",
                styles.dim.apply_to(&ts),
                styles.red.apply_to("\u{2717}"),
                styles.bold.apply_to(label),
                styles.red.apply_to(error),
            ))
        }
        "agent.message" => {
            let stage = str_field(&envelope, "node_id").unwrap_or("?");
            let model = prop_str_field(&envelope, "model").unwrap_or("?");
            let text = prop_str_field(&envelope, "text").unwrap_or("");
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
        "agent.tool.started" => {
            let tool = prop_str_field(&envelope, "tool_name").unwrap_or("?");
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
        "agent.tool.completed" => {
            let tool = prop_str_field(&envelope, "tool_name").unwrap_or("?");
            let is_error = prop_field(&envelope, "is_error")
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
        "edge.selected" => {
            let to = prop_str_field(&envelope, "to_node").unwrap_or("?");
            let reason = prop_str_field(&envelope, "reason").unwrap_or("?");
            let condition = prop_str_field(&envelope, "condition");
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
        "sandbox.ready" => {
            let provider = prop_str_field(&envelope, "provider").unwrap_or("?");
            let duration = format_duration_ms(prop_field(&envelope, "duration_ms"));
            Some(format!(
                "{}   Sandbox: {}  {}",
                styles.dim.apply_to(&ts),
                provider,
                styles.dim.apply_to(&duration),
            ))
        }
        "setup.completed" => {
            let count = prop_field(&envelope, "command_count").and_then(serde_json::Value::as_u64);
            let duration = format_duration_ms(prop_field(&envelope, "duration_ms"));
            Some(match count {
                Some(count) => format!(
                    "{}   Setup: {} commands  {}",
                    styles.dim.apply_to(&ts),
                    count,
                    styles.dim.apply_to(&duration),
                ),
                None => format!(
                    "{}   Setup: {}",
                    styles.dim.apply_to(&ts),
                    styles.dim.apply_to(&duration),
                ),
            })
        }
        "agent.compaction.completed" => {
            let original = prop_field(&envelope, "original_turn_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let preserved = prop_field(&envelope, "preserved_turn_count")
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
        "parallel.started" => {
            let count = prop_field(&envelope, "branch_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            Some(format!(
                "{} {} Parallel  {} branches",
                styles.dim.apply_to(&ts),
                styles.bold_cyan.apply_to("\u{25b6}"),
                count,
            ))
        }
        "parallel.branch.started" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            Some(format!(
                "{}     {} {}",
                styles.dim.apply_to(&ts),
                styles.cyan.apply_to("\u{25b6}"),
                label,
            ))
        }
        "parallel.branch.completed" => {
            let label = str_field(&envelope, "node_label").unwrap_or("?");
            Some(format!(
                "{}     {} {}",
                styles.dim.apply_to(&ts),
                styles.green.apply_to("\u{2713}"),
                label,
            ))
        }
        "parallel.completed" => {
            let duration = format_duration_ms(prop_field(&envelope, "duration_ms"));
            Some(format!(
                "{} {} Parallel  {}",
                styles.dim.apply_to(&ts),
                styles.green.apply_to("\u{2713}"),
                duration,
            ))
        }
        "pull_request.created" => {
            let url = prop_str_field(&envelope, "pr_url").unwrap_or("?");
            let draft = prop_field(&envelope, "draft")
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
        "pull_request.failed" => {
            let error = prop_str_field(&envelope, "error").unwrap_or("unknown error");
            Some(format!(
                "{} {} {}",
                styles.dim.apply_to(&ts),
                styles.bold_red.apply_to("PR failed:"),
                styles.red.apply_to(error),
            ))
        }
        "retro.completed" => {
            let duration = format_duration_ms(prop_field(&envelope, "duration_ms"));
            Some(format!(
                "{} {} Retro  {}",
                styles.dim.apply_to(&ts),
                styles.green.apply_to("\u{2713}"),
                duration,
            ))
        }
        "retro.failed" => {
            let error = prop_str_field(&envelope, "error").unwrap_or("unknown error");
            let duration = format_duration_ms(prop_field(&envelope, "duration_ms"));
            Some(format!(
                "{} {} Retro  {}  {}",
                styles.dim.apply_to(&ts),
                styles.bold_red.apply_to("\u{2717}"),
                duration,
                styles.red.apply_to(error),
            ))
        }
        "retro.started" => Some(format!(
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

fn prop_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    value.get("properties")?.get(key)
}

fn prop_str_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    prop_field(value, key)?.as_str()
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
    match value {
        Some(value) => {
            if let Some(usd_micros) = value.as_i64() {
                if usd_micros > 0 {
                    return format_usd_micros(usd_micros);
                }
            }
            let cost = value.as_f64().unwrap_or(0.0);
            if cost > 0.0 {
                format!("${cost:.2}")
            } else {
                String::new()
            }
        }
        None => String::new(),
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
    let tool_name = prop_str_field(envelope, "tool_name")?;
    let arguments = prop_field(envelope, "arguments")?;
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
            r#"{"ts":"2026-01-01T11:00:00Z","event":"stage.started"}"#.to_string(),
            r#"{"ts":"2026-01-01T12:30:00Z","event":"stage.completed"}"#.to_string(),
            r#"{"ts":"2026-01-01T13:00:00Z","event":"run.completed"}"#.to_string(),
        ];
        let result = apply_filters(&lines, Some(&cutoff), None);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn raw_lines_pass_through_verbatim() {
        let lines = vec![
            r#"{"ts":"2026-01-01T12:00:00Z","event":"stage.started","node_label":"plan"}"#
                .to_string(),
        ];
        let result = apply_filters(&lines, None, None);
        assert_eq!(result, lines);
    }

    #[test]
    fn pretty_stage_started() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:09Z","event":"stage.started","node_label":"plan","node_id":"plan","properties":{"index":0}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("plan"), "got: {result}");
        assert!(result.contains("\u{25b6}"), "got: {result}");
    }

    #[test]
    fn pretty_stage_completed() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:15Z","event":"stage.completed","node_label":"plan","properties":{"duration_ms":8000,"status":"success","usage":{"cost":0.12,"input_tokens":10000,"output_tokens":5200}}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("plan"), "got: {result}");
        assert!(result.contains("$0.12"), "got: {result}");
        assert!(result.contains("8s"), "got: {result}");
        assert!(result.contains("15.2k toks"), "got: {result}");
    }

    #[test]
    fn pretty_assistant_message() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:12Z","event":"agent.message","node_id":"plan","properties":{"model":"claude-opus-4-6","text":"I'll start by reading the code.","usage":{"input_tokens":100,"output_tokens":50},"tool_call_count":0}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("plan"), "got: {result}");
        assert!(result.contains("claude-opus-4-6"), "got: {result}");
        assert!(result.contains("reading the code"), "got: {result}");
    }

    #[test]
    fn pretty_tool_call_started() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:12Z","event":"agent.tool.started","properties":{"tool_name":"read_file","tool_call_id":"tc_1","arguments":{"path":"src/main.rs"}}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("read_file"), "got: {result}");
        assert!(result.contains("src/main.rs"), "got: {result}");
    }

    #[test]
    fn pretty_skips_noise_events() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:12Z","event":"agent.text.delta","properties":{"delta":"hello"}}"#;
        assert!(format_event_pretty(line, &styles).is_none());
    }

    #[test]
    fn pretty_skips_assistant_output_replace_noise_event() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:12Z","event":"agent.output.replace","properties":{"text":""}}"#;
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
        let line = r#"{"ts":"2026-01-01T14:23:01Z","run_id":"abc123","event":"run.started","properties":{"name":"smoke"}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("smoke"), "got: {result}");
        assert!(result.contains("abc123"), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_started_with_goal() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:01Z","run_id":"abc123","event":"run.started","properties":{"name":"smoke","goal":"Fix the bug"}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("smoke"), "got: {result}");
        assert!(result.contains("abc123"), "got: {result}");
        assert!(result.contains("Fix the bug"), "got: {result}");
        assert!(result.contains('\n'), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_started_without_goal_no_extra_lines() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:01Z","run_id":"abc123","event":"run.started","properties":{"name":"smoke"}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(!result.contains('\n'), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_completed() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:32Z","run_id":"abc123","event":"run.completed","properties":{"duration_ms":25000,"status":"success","total_usd_micros":570000,"billing":{"input_tokens":5000,"output_tokens":2000,"total_tokens":7000,"cache_read_tokens":3000,"cache_write_tokens":500,"reasoning_tokens":800}}}"#;
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
        let line = r#"{"ts":"2026-01-01T14:23:32Z","run_id":"abc123","event":"run.completed","properties":{"duration_ms":25000,"total_cost":0.57}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("SUCCESS"), "got: {result}");
        assert!(result.contains("25s"), "got: {result}");
        assert!(result.contains("$0.57"), "got: {result}");
        assert!(!result.contains("Tokens:"), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_completed_fail_status() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:32Z","event":"run.completed","properties":{"duration_ms":25000,"status":"fail"}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("FAIL"), "got: {result}");
    }

    #[test]
    fn pretty_pull_request_created() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"pull_request.created","properties":{"pr_url":"https://github.com/owner/repo/pull/42","pr_number":42,"draft":false}}"#;
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
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"pull_request.created","properties":{"pr_url":"https://github.com/owner/repo/pull/42","pr_number":42,"draft":true}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("Draft PR:"), "got: {result}");
    }

    #[test]
    fn pretty_pull_request_failed() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"pull_request.failed","properties":{"error":"auth token expired"}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("PR failed:"), "got: {result}");
        assert!(result.contains("auth token expired"), "got: {result}");
    }

    #[test]
    fn pretty_run_notice_warn() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"run.notice","properties":{"level":"warn","code":"sandbox_cleanup_failed","message":"sandbox cleanup failed: boom"}}"#;
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
        let line = r#"{"ts":"2026-01-01T14:25:00Z","event":"run.notice","properties":{"level":"error","code":"launch_failed","message":"failed to start engine"}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("Error:"), "got: {result}");
        assert!(result.contains("failed to start engine"), "got: {result}");
        assert!(result.contains("[launch_failed]"), "got: {result}");
    }

    #[test]
    fn pretty_workflow_run_failed() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:32Z","run_id":"abc123","event":"run.failed","properties":{"error":"sandbox timeout"}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("Failed"), "got: {result}");
        assert!(result.contains("sandbox timeout"), "got: {result}");
    }

    #[test]
    fn pretty_setup_completed_without_command_count() {
        let styles = no_color_styles();
        let line = r#"{"ts":"2026-01-01T14:23:32Z","event":"setup.completed","properties":{"duration_ms":800}}"#;
        let result = format_event_pretty(line, &styles).unwrap();
        assert!(result.contains("Setup:"), "got: {result}");
        assert!(result.contains("800ms"), "got: {result}");
        assert!(!result.contains("0 commands"), "got: {result}");
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
