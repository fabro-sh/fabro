//! A Petri run's stream as the CLI reads it: `run events` prints the
//! envelope raw or pretty, and `run attach` follows it live from a cursor.
//!
//! The stream is `GET /runs/{id}/events` for a run whose spec names Petri:
//! one ordered delivery of Petri's own events and Fabro's platform records
//! in the `RunStreamItem` envelope, addressed by `stream_seq`. The CLI
//! never names a Petri type; it reads the item as JSON through
//! [`PetriItem`], which knows where Petri's contract keeps the event name,
//! the subject and the parsed progress payloads.

#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `run events` output: blocking std::io::Write is the intended mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `run events` output: streams lines to std::io::stdout directly"
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context as _, Result};
use chrono::{DateTime, TimeZone as _, Utc};
use fabro_redact::redact_jsonl_line;
use fabro_types::{RunId, RunStreamItem, RunStreamItemKind};
use fabro_util::terminal::Styles;
use serde_json::Value;
use tokio::time;
use tracing::debug;

use crate::args::EventsArgs;
use crate::server_client;
use crate::shared::{format_duration_ms, format_usd_micros};

/// How long a follower waits before it reconnects after the server ended
/// the stream while the run was still active.
const FOLLOW_RECONNECT_DELAY: Duration = Duration::from_millis(200);

/// One stream item read as Petri's contract lays it out.
#[derive(Clone, Copy)]
pub(crate) struct PetriItem<'a> {
    pub(crate) item: &'a RunStreamItem,
}

impl<'a> PetriItem<'a> {
    pub(crate) fn new(item: &'a RunStreamItem) -> Self {
        Self { item }
    }

    pub(crate) fn is_petri(self) -> bool {
        self.item.kind == RunStreamItemKind::Petri
    }

    /// The `<subject>.<verb>` name of a Petri event, or the kind of a
    /// platform record.
    pub(crate) fn name(self) -> Option<&'a str> {
        self.item.name()
    }

    pub(crate) fn recorded_at(self) -> DateTime<Utc> {
        millis(self.item.recorded_at)
    }

    fn value(self) -> &'a Value {
        &self.item.item
    }

    /// The recorded event's fields, beside its `event` tag.
    pub(crate) fn body(self) -> Option<&'a Value> {
        self.value().pointer("/record/body")
    }

    pub(crate) fn derived(self) -> Option<&'a Value> {
        self.value().get("derived")
    }

    /// Petri's reading of a `step.progress.recorded` payload.
    pub(crate) fn parsed(self) -> Option<&'a Value> {
        self.derived()?.get("parsed")
    }

    /// The `custom` payload of a `step.progress.recorded`, when it is one.
    pub(crate) fn custom(self) -> Option<&'a Value> {
        self.body()?.pointer("/ev/custom")
    }

    /// A `step.progress.recorded` log line: `(stream, line)`.
    pub(crate) fn log_line(self) -> Option<(&'a str, &'a str)> {
        let log = self.body()?.pointer("/ev/log")?;
        Some((log.get("stream")?.as_str()?, log.get("line")?.as_str()?))
    }

    pub(crate) fn subject(self) -> Option<&'a Value> {
        self.value().get("subject")
    }

    pub(crate) fn node(self) -> Option<&'a Value> {
        self.subject()?.get("node")
    }

    pub(crate) fn node_name(self) -> Option<&'a str> {
        self.node()?.get("name")?.as_str()
    }

    /// The node's display label: its `meta.label`, else its name.
    pub(crate) fn node_label(self) -> Option<&'a str> {
        let node = self.node()?;
        node.pointer("/meta/label")
            .and_then(Value::as_str)
            .filter(|label| !label.is_empty())
            .or_else(|| node.get("name")?.as_str())
    }

    pub(crate) fn node_kind(self) -> Option<&'a str> {
        self.node()?.pointer("/meta/kind")?.as_str()
    }

    /// Whether the subject's node is a logical stage: not a lowering node
    /// (`synthetic`) and not a fork's `parallel.branch` delegate.
    pub(crate) fn is_shown_stage(self) -> bool {
        let Some(node) = self.node() else {
            return false;
        };
        let synthetic = node
            .pointer("/meta/synthetic")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        !synthetic && self.node_kind() != Some("parallel.branch")
    }

    pub(crate) fn visit(self) -> u64 {
        self.subject()
            .and_then(|subject| subject.get("visit"))
            .and_then(Value::as_u64)
            .unwrap_or(1)
    }

    pub(crate) fn firing(self) -> Option<u64> {
        self.subject()?.get("firing")?.as_u64()
    }

    pub(crate) fn execution(self) -> Option<u64> {
        self.value().pointer("/context/execution")?.as_u64()
    }

    /// The stage key: `(execution, firing)`.
    pub(crate) fn stage_key(self) -> Option<String> {
        Some(format!("{}:{}", self.execution()?, self.firing()?))
    }

    /// The stored platform record, tagged by `kind`.
    pub(crate) fn platform_record(self) -> Option<&'a Value> {
        (!self.is_petri())
            .then(|| self.value().get("record"))
            .flatten()
    }

    pub(crate) fn str_at(self, pointer: &str) -> Option<&'a str> {
        self.value().pointer(pointer)?.as_str()
    }

    /// The condition the applied route matched, as written: the edge a
    /// `route.applied` record names is a key into the subject node's
    /// `meta.edges`, whose entry carries the edge's `condition` when it
    /// has one.
    pub(crate) fn matched_condition(self) -> Option<&'a str> {
        let edge = self.body()?.get("edge")?.as_u64()?;
        self.node()?
            .pointer(&format!("/meta/edges/{edge}/condition"))?
            .as_str()
    }
}

/// Whether the item is the platform record of the run's terminal lifecycle
/// transition, which ends the attached stream.
pub(crate) fn is_terminal_lifecycle(item: &RunStreamItem) -> bool {
    let view = PetriItem::new(item);
    view.platform_record().is_some_and(|record| {
        record["kind"].as_str() == Some("run.lifecycle")
            && matches!(
                record["transition"].as_str(),
                Some("succeeded" | "failed" | "dead")
            )
    })
}

/// The exit code the item decides, if it is one that ends the run: the
/// engine's `run.finished`, or the platform record of the terminal
/// lifecycle transition.
pub(crate) fn exit_code_of(item: &RunStreamItem) -> Option<u8> {
    let view = PetriItem::new(item);
    if view.is_petri() {
        if view.name() != Some("run.finished") {
            return None;
        }
        return Some(match view.body()?.get("status")?.as_str()? {
            "success" => 0,
            _ => 1,
        });
    }
    let record = view.platform_record()?;
    if record["kind"].as_str() != Some("run.lifecycle") {
        return None;
    }
    match record["transition"].as_str()? {
        "succeeded" => Some(0),
        "failed" | "dead" => Some(1),
        _ => None,
    }
}

/// The question a `step.progress.recorded` carries, when Petri parsed one:
/// its id and text.
pub(crate) fn question_of(item: &RunStreamItem) -> Option<(&str, &str)> {
    let view = PetriItem::new(item);
    let parsed = view.parsed()?;
    if parsed.get("kind")?.as_str()? != "question" {
        return None;
    }
    let question = parsed.get("question")?;
    Some((
        question.get("id")?.as_str()?,
        question.get("text")?.as_str()?,
    ))
}

/// Whether the item closes the question with this id: a delivered answer,
/// its expiry, or Fabro's record of who answered.
pub(crate) fn resolves_question(item: &RunStreamItem, question_id: &str) -> bool {
    let view = PetriItem::new(item);
    if let Some(record) = view.platform_record() {
        return record["kind"].as_str() == Some("interview.answered")
            && record["question"].as_str() == Some(question_id);
    }
    match view.name() {
        Some("control.requested") => view.str_at("/derived/answer/question") == Some(question_id),
        Some("step.progress.recorded") => view.parsed().is_some_and(|parsed| {
            parsed["kind"].as_str() == Some("question_expired")
                && parsed["question"].as_str() == Some(question_id)
        }),
        _ => false,
    }
}

/// The raw line `run events` prints for an item: the envelope as JSON,
/// redacted.
pub(crate) fn raw_line(item: &RunStreamItem) -> Result<String> {
    let line = serde_json::to_string(item)?;
    Ok(redact_jsonl_line(&line))
}

/// What the pretty printer remembers between items: when each firing
/// started, and when the run did.
#[derive(Default)]
pub(crate) struct PrettyState {
    clock: StageClock,
}

/// When each firing's visit started, by stage key, so its completion can
/// show a duration.
#[derive(Default)]
pub(crate) struct StageClock {
    starts:    HashMap<String, u64>,
    run_start: Option<u64>,
}

impl StageClock {
    /// Note the item and answer how long its stage or the run has been
    /// running, when the item ends one.
    pub(crate) fn observe(&mut self, item: &RunStreamItem) -> Option<u64> {
        let view = PetriItem::new(item);
        match view.name()? {
            "run.started" if view.is_petri() => {
                self.run_start = Some(item.recorded_at);
                None
            }
            "visit.started" => {
                if let Some(key) = view.stage_key() {
                    self.starts.insert(key, item.recorded_at);
                }
                None
            }
            "visit.completed" | "branch.completed" => {
                let key = view.stage_key()?;
                let start = self.starts.remove(&key)?;
                Some(item.recorded_at.saturating_sub(start))
            }
            "run.finished" if view.is_petri() => {
                Some(item.recorded_at.saturating_sub(self.run_start?))
            }
            _ => None,
        }
    }
}

/// The pretty line for an item, or nothing for one the terminal does not
/// show. Petri events render by `<subject>.<verb>` with the stage's label;
/// platform records by their kind.
pub(crate) fn format_pretty(
    item: &RunStreamItem,
    styles: &Styles,
    state: &mut PrettyState,
) -> Option<String> {
    let elapsed = state.clock.observe(item);
    let view = PetriItem::new(item);
    let ts = view.recorded_at().format("%H:%M:%S").to_string();
    let ts = styles.dim.apply_to(&ts).to_string();
    if let Some(record) = view.platform_record() {
        return format_platform_record(&ts, record, styles);
    }
    // A revisited node shows its visit beside its label, as a stage id does.
    let label = match (view.node_label(), view.visit()) {
        (Some(label), 1) => label.to_string(),
        (Some(label), visit) => format!("{label}@{visit}"),
        (None, _) => "?".to_string(),
    };
    let label = label.as_str();
    match view.name()? {
        "run.started" => Some(format!(
            "{ts}   {}",
            styles.dim.apply_to("Engine: petri run started")
        )),
        "visit.started" if view.is_shown_stage() => Some(format!(
            "{ts} {} {}",
            styles.bold_cyan.apply_to("\u{25b6}"),
            styles.bold.apply_to(label),
        )),
        "visit.completed" if view.is_shown_stage() => {
            let derived = view.derived()?;
            let outcome = derived.get("outcome")?;
            let executed = derived
                .get("executed")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let status = outcome.get("status").and_then(Value::as_str).unwrap_or("?");
            let duration = format_duration_ms(elapsed.unwrap_or(0));
            if !executed || status == "skipped" {
                return Some(format!(
                    "{ts} {} {}  {}",
                    styles.dim.apply_to("\u{2298}"),
                    styles.bold.apply_to(label),
                    styles.dim.apply_to("skipped"),
                ));
            }
            match status {
                "success" | "partial_success" => {
                    let mut line = format!(
                        "{ts} {} {}",
                        styles.green.apply_to("\u{2713}"),
                        styles.bold.apply_to(label),
                    );
                    if status == "partial_success" {
                        let _ = write!(line, "  {}", styles.yellow.apply_to("partial"));
                    }
                    let _ = write!(line, "  {duration}");
                    if let Some(usage) = usage_summary(outcome, styles) {
                        let _ = write!(line, "  {usage}");
                    }
                    Some(line)
                }
                other => {
                    let error = outcome
                        .pointer("/failure/message")
                        .and_then(Value::as_str)
                        .unwrap_or(other);
                    Some(format!(
                        "{ts} {} {}  {}",
                        styles.red.apply_to("\u{2717}"),
                        styles.bold.apply_to(label),
                        styles.red.apply_to(error),
                    ))
                }
            }
        }
        "retry.scheduled" => {
            let derived = view.derived()?;
            let attempt = derived.get("next_attempt").and_then(Value::as_u64)?;
            let delay = derived
                .pointer("/base_delay/secs")
                .and_then(Value::as_u64)
                .map(|secs| secs.saturating_mul(1000))
                .or_else(|| derived.get("base_delay").and_then(Value::as_u64))
                .unwrap_or(0);
            Some(format!(
                "{ts} {} {}: retrying (attempt {attempt}, delay {})",
                styles.yellow.apply_to("\u{21bb}"),
                label,
                format_duration_ms(delay),
            ))
        }
        "route.applied" => {
            let derived = view.derived()?;
            let target = derived.pointer("/target/name").and_then(Value::as_str)?;
            let transition = derived
                .get("transition")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let back = derived
                .get("back")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let detail = if back { " (loop)" } else { "" };
            // The condition the edge matched, when it has one.
            let condition = view
                .matched_condition()
                .map_or_else(String::new, |condition| format!(" when {condition}"));
            // Petri records the route after the next visit started, so the
            // line names both ends of the edge.
            Some(format!(
                "{ts}    {} {} {} {}{}{}",
                styles.dim.apply_to(view.node_name().unwrap_or("?")),
                styles.dim.apply_to("\u{2192}"),
                target,
                styles.dim.apply_to(&transition),
                styles.dim.apply_to(detail),
                styles.dim.apply_to(&condition),
            ))
        }
        "fork.started" => {
            let branches = view
                .derived()?
                .get("branches")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Some(format!(
                "{ts} {} {}  {}",
                styles.bold_cyan.apply_to("\u{2442}"),
                styles.bold.apply_to(label),
                styles.dim.apply_to(format!("{branches} branches")),
            ))
        }
        "branch.completed" => {
            let result = view.derived()?.get("result")?;
            let name = result
                .pointer("/node/name")
                .and_then(Value::as_str)
                .unwrap_or(label);
            let status = result.get("status").and_then(Value::as_str).unwrap_or("?");
            let (glyph, style) = if status == "success" {
                ("\u{2713}", &styles.green)
            } else {
                ("\u{2717}", &styles.red)
            };
            Some(format!(
                "{ts}    {} branch {}  {}  {}",
                style.apply_to(glyph),
                name,
                styles.dim.apply_to(status),
                styles
                    .dim
                    .apply_to(format_duration_ms(elapsed.unwrap_or(0))),
            ))
        }
        "fork.completed" => {
            let derived = view.derived()?;
            let results = derived
                .get("results")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let disposition = derived
                .get("disposition")
                .and_then(Value::as_str)
                .unwrap_or("joined");
            Some(format!(
                "{ts}    {} {}  {}",
                styles.dim.apply_to("\u{2442}"),
                styles
                    .dim
                    .apply_to(format!("{results} branches {disposition}")),
                styles.bold.apply_to(label),
            ))
        }
        "step.progress.recorded" => format_progress(&ts, view, label, styles),
        "control.requested" => {
            if let Some(interrupt) = view.body()?.pointer("/ctl/deliver/$interrupt") {
                let text = interrupt
                    .get("steer")
                    .and_then(Value::as_str)
                    .map(|text| format!(": {text}"))
                    .unwrap_or_default();
                return Some(format!(
                    "{ts} {} {}{}",
                    styles.yellow.apply_to("\u{23f8} Interrupt"),
                    styles.bold.apply_to(label),
                    text,
                ));
            }
            let answer = view.derived()?.get("answer")?;
            let value = answer
                .get("choice")
                .or_else(|| answer.get("text"))
                .and_then(Value::as_str)
                .map_or_else(|| answer.to_string(), str::to_string);
            let late = !view
                .derived()?
                .get("deliverable")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let suffix = if late { " (late)" } else { "" };
            Some(format!(
                "{ts}    {} answered: {}{}",
                styles.dim.apply_to("\u{21b3}"),
                value,
                styles.dim.apply_to(suffix),
            ))
        }
        "invocation.cancel.requested" => {
            let reason = view
                .body()?
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("?");
            Some(format!(
                "{ts} {} {}",
                styles.bold_red.apply_to("\u{2717} Cancel requested"),
                styles.dim.apply_to(reason),
            ))
        }
        "run.finished" => {
            let status = view.body()?.get("status").and_then(Value::as_str)?;
            let duration = format_duration_ms(elapsed.unwrap_or(0));
            Some(match status {
                "success" => format!(
                    "{ts} {} {}",
                    styles.bold_green.apply_to("\u{2713} SUCCEEDED"),
                    styles.bold.apply_to(&duration),
                ),
                "cancelled" => format!(
                    "{ts} {} {}",
                    styles.bold_red.apply_to("\u{2717} CANCELLED"),
                    styles.bold.apply_to(&duration),
                ),
                _ => format!(
                    "{ts} {} {}",
                    styles.bold_red.apply_to("\u{2717} FAILED"),
                    styles.bold.apply_to(&duration),
                ),
            })
        }
        _ => None,
    }
}

/// The token and cost summary of a finished visit, from the Pebble or
/// prompt usage in its metrics.
fn usage_summary(outcome: &Value, styles: &Styles) -> Option<String> {
    let custom = outcome.pointer("/metrics/custom")?;
    let usage = custom
        .get("pebble.usage")
        .or_else(|| custom.get("prompt.usage"))?;
    let tokens = usage.get("tokens")?;
    let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
    let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
    let total = input.saturating_add(output);
    let mut parts = Vec::new();
    if let Some(cost) = usage.pointer("/cost/usd_micros").and_then(Value::as_u64) {
        parts.push(format_usd_micros(cost));
    }
    if total > 0 {
        parts.push(format!("{total} toks"));
    }
    (!parts.is_empty()).then(|| styles.dim.apply_to(parts.join("  ")).to_string())
}

/// The line for a `step.progress.recorded`: a question, a log line, an
/// agent envelope, a prompt's completion.
fn format_progress(ts: &str, view: PetriItem<'_>, label: &str, styles: &Styles) -> Option<String> {
    if let Some(parsed) = view.parsed() {
        match parsed.get("kind").and_then(Value::as_str) {
            Some("question") => {
                let question = parsed.get("question")?;
                let text = question.get("text").and_then(Value::as_str).unwrap_or("");
                let mut line = format!(
                    "{ts} {} {}: {}",
                    styles.yellow.apply_to("?"),
                    styles.bold.apply_to(label),
                    text,
                );
                if let Some(options) = question.get("options").and_then(Value::as_array) {
                    let labels: Vec<&str> = options
                        .iter()
                        .filter_map(|option| option.get("label").and_then(Value::as_str))
                        .collect();
                    if !labels.is_empty() {
                        let _ = write!(line, "  {}", styles.dim.apply_to(labels.join("  ")));
                    }
                }
                return Some(line);
            }
            Some("question_expired") => {
                let default = parsed
                    .get("default")
                    .and_then(Value::as_str)
                    .map_or_else(String::new, |default| format!(" (default {default})"));
                return Some(format!(
                    "{ts}    {} question expired{}",
                    styles.dim.apply_to("\u{21b3}"),
                    styles.dim.apply_to(&default),
                ));
            }
            _ => {}
        }
    }
    if let Some((_, line)) = view.log_line() {
        return Some(format!(
            "{ts}    {} {}",
            styles.dim.apply_to("\u{2502}"),
            styles.dim.apply_to(line),
        ));
    }
    let custom = view.custom()?;
    match custom.get("kind").and_then(Value::as_str)? {
        "pebble" => format_envelope(ts, custom.get("event")?, label, styles),
        "attractor.prompt.completed" => {
            let response = custom.get("response").and_then(Value::as_str).unwrap_or("");
            let header = format!("{ts} {} {}", "\u{1f4ac}", styles.bold.apply_to(label));
            let body = indented(styles, response, "            ");
            Some(format!("{header}\n{body}\n"))
        }
        "attractor.tools" => {
            // The tools one native session was offered, once per session.
            let count = custom
                .get("tools")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let noun = if count == 1 { "tool" } else { "tools" };
            Some(format!(
                "{ts}    {} {}",
                styles.dim.apply_to("\u{2699}"),
                styles.dim.apply_to(format!("{count} {noun} available")),
            ))
        }
        "attractor.turn.interrupted" => {
            let backend = custom.get("backend").and_then(Value::as_str).unwrap_or("?");
            Some(format!(
                "{ts}    {} {}",
                styles.dim.apply_to("\u{21b3}"),
                styles.dim.apply_to(format!("turn interrupted ({backend})")),
            ))
        }
        "attractor.checkout" => {
            let repository = custom
                .get("repository")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let commit = custom.get("commit").and_then(Value::as_str).unwrap_or("");
            Some(format!(
                "{ts}   Checkout: {} {}",
                repository,
                styles.dim.apply_to(short_sha(commit)),
            ))
        }
        _ => None,
    }
}

/// The line for a Pebble coding-agent envelope: the assistant's text, a
/// tool call's start and end.
fn format_envelope(ts: &str, envelope: &Value, label: &str, styles: &Styles) -> Option<String> {
    let event = envelope.get("event")?.as_object()?;
    let (variant, fields) = event.iter().next()?;
    match variant.as_str() {
        "AssistantMessage" => {
            let model = fields.get("model").and_then(Value::as_str).unwrap_or("?");
            let text = fields.get("text").and_then(Value::as_str).unwrap_or("");
            let header = format!(
                "{ts} {} {} {}",
                "\u{1f4ac}",
                styles.bold.apply_to(label),
                styles.dim.apply_to(format!("[{model}]")),
            );
            let body = indented(styles, text, "            ");
            Some(format!("{header}\n{body}\n"))
        }
        "ToolCallStarted" => {
            let tool = fields
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("?");
            Some(format!(
                "{ts}    {} {}",
                styles.dim.apply_to("\u{2699}"),
                styles.dim.apply_to(tool),
            ))
        }
        "ToolCallCompleted" => {
            let tool = fields
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let is_error = fields
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (glyph, style) = if is_error {
                ("\u{2717}", &styles.red)
            } else {
                ("\u{2713}", &styles.green)
            };
            Some(format!("{ts}    {} {}", style.apply_to(glyph), tool))
        }
        _ => None,
    }
}

/// The line for a platform record, by its kind.
fn format_platform_record(ts: &str, record: &Value, styles: &Styles) -> Option<String> {
    let kind = record.get("kind")?.as_str()?;
    match kind {
        "run.created" => {
            let spec = record.get("spec");
            let name = record
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| spec?.pointer("/settings/workflow/name")?.as_str())
                .or_else(|| spec?.get("workflow_slug")?.as_str())
                .unwrap_or("Run");
            let run_id = spec
                .and_then(|spec| spec.get("run_id"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let header = format!(
                "{ts} {} {}  {}",
                styles.bold_cyan.apply_to("\u{25b6}"),
                styles.bold.apply_to(name),
                styles.dim.apply_to(run_id),
            );
            match spec
                .and_then(|spec| spec.pointer("/settings/run/goal"))
                .and_then(Value::as_str)
            {
                Some(goal) if !goal.is_empty() => {
                    let body = indented(styles, goal, "            ");
                    Some(format!("{header}\n{body}\n"))
                }
                _ => Some(header),
            }
        }
        "run.lifecycle" => {
            let transition = record.get("transition").and_then(Value::as_str)?;
            let reason = record
                .get("reason")
                .and_then(Value::as_str)
                .map_or_else(String::new, |reason| format!(": {reason}"));
            Some(format!(
                "{ts}   {}",
                styles
                    .dim
                    .apply_to(format!("\u{00b7} {transition}{reason}"))
            ))
        }
        "run.notice" => {
            let level = record
                .get("level")
                .and_then(Value::as_str)
                .unwrap_or("info");
            let code = record.get("code").and_then(Value::as_str).unwrap_or("");
            let message = record.get("message").and_then(Value::as_str).unwrap_or("");
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
            Some(format!("{ts} {label} {message}{code_suffix}"))
        }
        "checkpoint" => {
            let sha = record
                .get("git_commit_sha")
                .and_then(Value::as_str)
                .map_or_else(|| "(no commit)".to_string(), short_sha);
            Some(format!(
                "{ts}    {} {}",
                styles.dim.apply_to("\u{2398} Checkpoint"),
                styles.dim.apply_to(sha),
            ))
        }
        "pull_request.created" => {
            let url = record
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let draft = record
                .get("draft")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let suffix = if draft { " (draft)" } else { "" };
            Some(format!(
                "{ts}   Pull request: {}{}",
                url,
                styles.dim.apply_to(suffix)
            ))
        }
        "interview.answered" => {
            let principal = record.get("principal");
            let who = principal
                .and_then(|principal| principal.get("login"))
                .or_else(|| principal?.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            Some(format!(
                "{ts}    {} answered by {}",
                styles.dim.apply_to("\u{21b3}"),
                styles.dim.apply_to(who),
            ))
        }
        "run.title" => {
            let title = record.get("title").and_then(Value::as_str).unwrap_or("");
            Some(format!("{ts}   Title: {title}"))
        }
        "run.branch" => {
            let branch = record
                .get("run_branch")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let base = record
                .get("base_sha")
                .and_then(Value::as_str)
                .map_or_else(String::new, |sha| format!(" from {}", short_sha(sha)));
            Some(format!(
                "{ts}   Branch: {}{}",
                branch,
                styles.dim.apply_to(&base)
            ))
        }
        "git.identity" => {
            // The identity's fields are flattened into the record.
            let identity = record;
            let name = identity.get("name").and_then(Value::as_str).unwrap_or("?");
            let email = identity.get("email").and_then(Value::as_str).unwrap_or("?");
            let source = identity
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("?");
            Some(format!(
                "{ts}   Git identity: {name} <{email}>  {}",
                styles.dim.apply_to(source)
            ))
        }
        "run.diff" => {
            let summary = record.get("diff_summary")?;
            let count = |key: &str| summary.get(key).and_then(Value::as_i64).unwrap_or(0);
            Some(format!(
                "{ts}   Diff: {} in {} file(s)",
                styles
                    .dim
                    .apply_to(format!("+{} -{}", count("additions"), count("deletions"))),
                count("files_changed")
            ))
        }
        "artifact.collected" => {
            let path = record.get("path").and_then(Value::as_str).unwrap_or("?");
            let bytes = record.get("bytes").and_then(Value::as_u64).unwrap_or(0);
            Some(format!(
                "{ts}    {} {path} {}",
                styles.dim.apply_to("\u{2398}"),
                styles.dim.apply_to(format!("({bytes} B)"))
            ))
        }
        other => Some(format!(
            "{ts}   {}",
            styles.dim.apply_to(format!("\u{00b7} {other}"))
        )),
    }
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn indented(styles: &Styles, text: &str, indent: &str) -> String {
    let wrap_width = Styles::terminal_width().saturating_sub(indent.len());
    styles
        .render_markdown_width(text, wrap_width)
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn millis(recorded_at: u64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(i64::try_from(recorded_at).unwrap_or(i64::MAX))
        .single()
        .unwrap_or_default()
}

/// `run events` for a Petri run: the stream, filtered by `--since` and
/// `--tail`, raw or pretty, then followed live when asked.
pub(crate) async fn print_events(
    client: &server_client::Client,
    run_id: &RunId,
    args: &EventsArgs,
    since: Option<DateTime<Utc>>,
    pretty: bool,
    styles: &Styles,
) -> Result<()> {
    let items = client
        .list_run_stream(run_id, 0)
        .await
        .context("Failed to list the run's stream")?;
    let last_seq = items.last().map_or(0, |item| item.stream_seq);
    let mut selected: Vec<&RunStreamItem> = items
        .iter()
        .filter(|item| since.is_none_or(|cutoff| PetriItem::new(item).recorded_at() >= cutoff))
        .collect();
    if let Some(tail) = args.tail {
        let start = selected.len().saturating_sub(tail);
        selected.drain(..start);
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut state = PrettyState::default();
    for item in selected {
        write_item(&mut out, item, pretty, styles, &mut state)?;
    }
    out.flush()?;

    if args.follow {
        Box::pin(follow(client, run_id, last_seq, pretty, styles, state)).await?;
    }
    Ok(())
}

fn write_item(
    out: &mut dyn Write,
    item: &RunStreamItem,
    pretty: bool,
    styles: &Styles,
    state: &mut PrettyState,
) -> Result<()> {
    if pretty {
        if let Some(line) = format_pretty(item, styles, state) {
            writeln!(out, "{line}")?;
        }
    } else {
        writeln!(out, "{}", raw_line(item)?)?;
    }
    Ok(())
}

/// Follow the stream live from `after`: attach, print each item, and on a
/// stream the server ended before the run's terminal record, reconnect
/// from the last `stream_seq` printed unless the run has concluded.
async fn follow(
    client: &server_client::Client,
    run_id: &RunId,
    after: u64,
    pretty: bool,
    styles: &Styles,
    mut state: PrettyState,
) -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut cursor = after;
    loop {
        let mut stream = client.attach_run_stream(run_id, Some(cursor)).await?;
        while let Some(item) = stream.next_item().await? {
            cursor = item.stream_seq;
            write_item(&mut out, &item, pretty, styles, &mut state)?;
            out.flush()?;
            if is_terminal_lifecycle(&item) {
                return Ok(());
            }
        }
        let run_state = client
            .get_run_state(run_id)
            .await
            .context("Failed to read run state from server while following the stream")?;
        if run_state.status.is_terminal() {
            // The server ended the stream after its grace: print what
            // landed since, if anything, and stop.
            for item in client.list_run_stream(run_id, cursor).await? {
                write_item(&mut out, &item, pretty, styles, &mut state)?;
            }
            out.flush()?;
            debug!("Run reached terminal status and the stream ended, stopping follow");
            return Ok(());
        }
        debug!(
            cursor,
            "the attached stream ended before the run did; reconnecting"
        );
        time::sleep(FOLLOW_RECONNECT_DELAY).await;
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::fixtures;
    use serde_json::json;

    use super::*;

    fn item(kind: RunStreamItemKind, stream_seq: u64, value: Value) -> RunStreamItem {
        RunStreamItem {
            run_id: fixtures::RUN_1,
            stream_seq,
            kind,
            id: stream_seq.to_string(),
            recorded_at: 1_789_706_579_000 + stream_seq * 1000,
            item: value,
        }
    }

    fn petri(stream_seq: u64, value: Value) -> RunStreamItem {
        item(RunStreamItemKind::Petri, stream_seq, value)
    }

    fn platform(stream_seq: u64, record: &Value) -> RunStreamItem {
        item(
            RunStreamItemKind::Platform,
            stream_seq,
            json!({"seq": stream_seq, "recorded_at": 0, "record": record}),
        )
    }

    fn subject(name: &str, kind: &str) -> Value {
        json!({
            "node": {"id": 2, "name": name, "kind": "attractor/x", "meta": {"label": name, "kind": kind}},
            "firing": 2, "visit": 1, "attempt": 1, "generation": 0, "branch": {"role": "none"}
        })
    }

    fn view_event(name: &str, node: &str, kind: &str, derived: Value) -> Value {
        let mut derived = derived;
        derived["event"] = json!(name);
        json!({
            "id": {"log": "execution", "execution": 0, "seq": 5, "index": 1},
            "origin": "derived",
            "context": {"invocation": 0, "execution": 0},
            "subject": subject(node, kind),
            "derived": derived
        })
    }

    #[test]
    fn a_stage_renders_its_start_and_its_end_with_the_elapsed_time() {
        let styles = Styles::new(false);
        let mut state = PrettyState::default();
        let started = petri(1, view_event("visit.started", "say", "command", json!({})));
        let completed = petri(
            4,
            view_event(
                "visit.completed",
                "say",
                "command",
                json!({
                    "outcome": {"status": "success", "metrics": {"duration_ms": 42}},
                    "executed": true, "attempts": 1
                }),
            ),
        );
        let start_line = format_pretty(&started, &styles, &mut state).expect("a start line");
        assert!(start_line.contains("\u{25b6} say"), "{start_line}");
        let end_line = format_pretty(&completed, &styles, &mut state).expect("an end line");
        assert!(end_line.contains("\u{2713} say"), "{end_line}");
        assert!(
            end_line.contains("  3s"),
            "the visit took three seconds: {end_line}"
        );
    }

    #[test]
    fn a_fork_delegate_is_not_a_stage_but_its_branch_completion_is_shown() {
        let styles = Styles::new(false);
        let mut state = PrettyState::default();
        let delegate = petri(
            1,
            view_event("visit.started", "a", "parallel.branch", json!({})),
        );
        assert!(format_pretty(&delegate, &styles, &mut state).is_none());
        let completed = petri(
            3,
            view_event(
                "branch.completed",
                "a",
                "parallel.branch",
                json!({
                    "result": {"node": {"name": "a"}, "status": "success"}
                }),
            ),
        );
        let line = format_pretty(&completed, &styles, &mut state).expect("a branch line");
        assert!(line.contains("branch a"), "{line}");
        assert!(line.contains("  2s"), "{line}");
    }

    #[test]
    fn a_platform_notice_and_the_terminal_lifecycle_record_render_by_kind() {
        let styles = Styles::new(false);
        let mut state = PrettyState::default();
        let notice = platform(
            2,
            &json!({"kind": "run.notice", "level": "warn", "code": "x.y", "message": "careful"}),
        );
        let line = format_pretty(&notice, &styles, &mut state).expect("a notice line");
        assert!(line.contains("Warning: careful [x.y]"), "{line}");
        let finished = platform(
            3,
            &json!({"kind": "run.lifecycle", "transition": "succeeded", "status": {"kind": "succeeded"}}),
        );
        assert!(is_terminal_lifecycle(&finished));
        assert_eq!(exit_code_of(&finished), Some(0));
        let line = format_pretty(&finished, &styles, &mut state).expect("a lifecycle line");
        assert!(line.contains("\u{00b7} succeeded"), "{line}");
    }

    #[test]
    fn a_question_is_found_and_closed_by_its_answer_or_by_who_answered() {
        let asked = petri(
            5,
            json!({
                "origin": "external",
                "context": {"invocation": 0, "execution": 0},
                "subject": subject("gate", "human"),
                "record": {"seq": 12, "body": {"event": "step.progress.recorded", "firing": 2,
                    "ev": {"custom": {"$question": {"id": "gate#2", "text": "Go?"}}}}},
                "derived": {"parsed": {"kind": "question", "question": {"id": "gate#2", "text": "Go?",
                    "options": [{"key": "Y", "label": "[Y] Yes"}], "kind": "yes_no"}}}
            }),
        );
        assert_eq!(question_of(&asked), Some(("gate#2", "Go?")));
        let styles = Styles::new(false);
        let mut state = PrettyState::default();
        let line = format_pretty(&asked, &styles, &mut state).expect("a question line");
        assert!(line.contains("? gate: Go?  [Y] Yes"), "{line}");

        let answered = petri(
            6,
            json!({
                "origin": "external",
                "context": {"invocation": 0, "execution": 0},
                "subject": subject("gate", "human"),
                "record": {"seq": 14, "body": {"event": "control.requested", "firing": 2,
                    "ctl": {"deliver": {"$answer": {"question": "gate#2", "choice": "N"}}}}},
                "derived": {"deliverable": true, "answer": {"question": "gate#2", "choice": "N"}}
            }),
        );
        assert!(resolves_question(&answered, "gate#2"));
        assert!(!resolves_question(&answered, "gate#3"));
        let who = platform(
            7,
            &json!({"kind": "interview.answered", "question": "gate#2", "principal": {"kind": "user", "login": "dev"}}),
        );
        assert!(resolves_question(&who, "gate#2"));
        let line = format_pretty(&who, &styles, &mut state).expect("an answered-by line");
        assert!(line.contains("answered by dev"), "{line}");
    }

    #[test]
    fn a_route_line_names_the_condition_the_edge_matched() {
        let styles = Styles::new(false);
        let mut state = PrettyState::default();
        let mut subject = subject("build", "command");
        subject["node"]["meta"]["edges"] = json!({
            "0": {"to": "ok", "label": null, "condition": "outcome=succeeded"},
            "1": {"to": "bad", "label": null}
        });
        let applied = |edge: u64| {
            petri(
                7,
                json!({
                    "origin": "core",
                    "context": {"invocation": 0, "execution": 0},
                    "subject": subject,
                    "record": {"seq": 9, "body": {"event": "route.applied", "kind": "edge",
                        "firing": 2, "group": 0, "edge": edge}},
                    "derived": {"target": {"name": if edge == 0 { "ok" } else { "bad" }},
                        "transition": "Continue", "back": false}
                }),
            )
        };
        let line = format_pretty(&applied(0), &styles, &mut state).expect("a route line");
        assert!(
            line.contains("build \u{2192} ok continue when outcome=succeeded"),
            "{line}"
        );
        let line = format_pretty(&applied(1), &styles, &mut state).expect("a route line");
        assert!(line.ends_with("build \u{2192} bad continue"), "{line}");
    }

    #[test]
    fn a_sessions_tool_list_renders_as_its_count() {
        let styles = Styles::new(false);
        let mut state = PrettyState::default();
        let listed = petri(
            8,
            json!({
                "origin": "external",
                "context": {"invocation": 0, "execution": 0},
                "subject": subject("work", "agent"),
                "record": {"seq": 10, "body": {"event": "step.progress.recorded", "firing": 2,
                    "ev": {"custom": {"kind": "attractor.tools", "session": "ses_1", "tools": [
                        {"name": "shell", "description": "Run a command", "source": {"kind": "native"}, "category": "builtin"},
                        {"name": "fabro_run_create", "description": "Create a run", "source": {"kind": "application"}, "category": "host"}
                    ]}}}}
            }),
        );
        let line = format_pretty(&listed, &styles, &mut state).expect("a tools line");
        assert!(line.contains("2 tools available"), "{line}");
    }

    #[test]
    fn an_interrupt_and_the_turn_it_stopped_render_by_kind() {
        let styles = Styles::new(false);
        let mut state = PrettyState::default();
        let interrupt = petri(
            8,
            json!({
                "origin": "external",
                "context": {"invocation": 0, "execution": 0},
                "subject": subject("work", "agent"),
                "record": {"seq": 20, "body": {"event": "control.requested", "firing": 2,
                    "ctl": {"deliver": {"$interrupt": {"steer": "stop and summarize"}}}}},
                "derived": {"deliverable": true}
            }),
        );
        let line = format_pretty(&interrupt, &styles, &mut state).expect("an interrupt line");
        assert!(
            line.contains("\u{23f8} Interrupt work: stop and summarize"),
            "{line}"
        );

        let plain = petri(
            9,
            json!({
                "origin": "external",
                "context": {"invocation": 0, "execution": 0},
                "subject": subject("work", "agent"),
                "record": {"seq": 21, "body": {"event": "control.requested", "firing": 2,
                    "ctl": {"deliver": {"$interrupt": {}}}}},
                "derived": {"deliverable": true}
            }),
        );
        let line = format_pretty(&plain, &styles, &mut state).expect("an interrupt line");
        assert!(line.ends_with("\u{23f8} Interrupt work"), "{line}");

        let stopped = petri(
            10,
            json!({
                "origin": "external",
                "context": {"invocation": 0, "execution": 0},
                "subject": subject("work", "agent"),
                "record": {"seq": 22, "body": {"event": "step.progress.recorded", "firing": 2,
                    "ev": {"custom": {"kind": "attractor.turn.interrupted", "node": "work",
                        "firing": 2, "attempt": 1, "backend": "api", "session": "s-1"}}}},
                "derived": {}
            }),
        );
        let line = format_pretty(&stopped, &styles, &mut state).expect("a stopped-turn line");
        assert!(line.contains("\u{21b3} turn interrupted (api)"), "{line}");
    }

    #[test]
    fn the_engine_finish_decides_the_exit_code() {
        let finished = petri(
            9,
            json!({
                "origin": "external", "context": {},
                "record": {"seq": 6, "body": {"event": "run.finished", "status": "failed"}}
            }),
        );
        assert_eq!(exit_code_of(&finished), Some(1));
        let styles = Styles::new(false);
        let mut state = PrettyState::default();
        let line = format_pretty(&finished, &styles, &mut state).expect("a finish line");
        assert!(line.contains("\u{2717} FAILED"), "{line}");
    }

    #[test]
    fn the_raw_line_is_the_envelope_as_json() {
        let notice = platform(
            2,
            &json!({"kind": "run.notice", "level": "info", "message": "m"}),
        );
        let line = raw_line(&notice).expect("a line");
        let value: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(value["stream_seq"], 2);
        assert_eq!(value["kind"], "platform");
        assert_eq!(value["item"]["record"]["kind"], "run.notice");
    }
}
