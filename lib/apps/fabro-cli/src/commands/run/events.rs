use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fabro_util::terminal::Styles;
use tracing::info;

use super::petri_stream;
use crate::args::EventsArgs;
use crate::command_context::CommandContext;

pub(crate) async fn run(
    args: &EventsArgs,
    styles: &Styles,
    base_ctx: &CommandContext,
) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.id;
    info!(run_id = %run_id, "Showing events");

    let since_cutoff = match &args.since {
        Some(value) => Some(parse_since(value)?),
        None => None,
    };

    // A Petri run's events are its stream, in the stream envelope.
    let state = client
        .get_run_state(&run_id)
        .await
        .context("Failed to read run state from server")?;
    let _ = state;
    let pretty = args.pretty && !ctx.json_output();
    Box::pin(petri_stream::print_events(
        client.as_ref(),
        &run_id,
        args,
        since_cutoff,
        pretty,
        styles,
    ))
    .await
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
    let num = i64::try_from(num_str.parse::<u64>().ok()?).ok()?;
    match unit {
        "s" => Some(chrono::Duration::seconds(num)),
        "m" => Some(chrono::Duration::minutes(num)),
        "h" => Some(chrono::Duration::hours(num)),
        "d" => Some(chrono::Duration::days(num)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parse_since_overflow_is_invalid() {
        assert!(parse_since("9223372036854775808s").is_err());
    }
}
