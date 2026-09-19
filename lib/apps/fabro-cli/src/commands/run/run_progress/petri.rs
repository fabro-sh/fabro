//! The progress lines a Petri run's stream items mean: the mapping from
//! Petri's `<subject>.<verb>` events and Fabro's platform records onto the
//! [`ProgressEvent`]s the renderer already draws for a legacy run.
//!
//! A stage is a firing whose node is a logical stage (`VIEWS.md`); its
//! display key is the node's name, as a legacy stage's `node_id` is. A
//! fork's `parallel.branch` delegates are the branches of the parallel
//! group, never stages of their own.

use fabro_types::{CodingAgentEvent, RunNoticeLevel, RunStreamItem, StageOutcome, StageTiming};
use serde_json::Value;

use super::event::{ProgressEvent, ProgressUsage, coding_progress_event};
use crate::commands::run::petri_stream::{PetriItem, StageClock};

/// What the mapping remembers between items: when each firing started.
#[derive(Default)]
pub(super) struct PetriProgressState {
    clock: StageClock,
}

/// The progress events one stream item means, in order.
pub(super) fn progress_events(
    item: &RunStreamItem,
    state: &mut PetriProgressState,
) -> Vec<ProgressEvent> {
    let elapsed = state.clock.observe(item);
    let view = PetriItem::new(item);
    if let Some(record) = view.platform_record() {
        return platform_progress_event(record).into_iter().collect();
    }
    let Some(name) = view.name() else {
        return Vec::new();
    };
    let node_id = view.node_name().unwrap_or("?").to_string();
    let label = view.node_label().unwrap_or("?").to_string();
    match name {
        "visit.started" => {
            if view.is_shown_stage() {
                vec![ProgressEvent::StageStarted {
                    node_id,
                    name: label,
                    script: None,
                }]
            } else if view.node_kind() == Some("parallel.branch") {
                vec![ProgressEvent::ParallelBranchStarted { branch: node_id }]
            } else {
                Vec::new()
            }
        }
        "visit.completed" if view.is_shown_stage() => {
            let Some(derived) = view.derived() else {
                return Vec::new();
            };
            let outcome = derived.get("outcome");
            let executed = derived
                .get("executed")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let status = outcome
                .and_then(|outcome| outcome.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let timing = StageTiming {
                wall_time_ms: elapsed.unwrap_or(0),
                ..StageTiming::default()
            };
            let completed =
                |status: &str, usage: Option<ProgressUsage>| ProgressEvent::StageCompleted {
                    node_id: node_id.clone(),
                    name: label.clone(),
                    timing,
                    status: status.to_string(),
                    usage,
                };
            if !executed || status == "skipped" {
                return vec![completed("skipped", None)];
            }
            match status {
                "success" => vec![completed("succeeded", outcome.and_then(usage_of))],
                "partial_success" => {
                    vec![completed("partially_succeeded", outcome.and_then(usage_of))]
                }
                "cancelled" => vec![completed("cancelled", None)],
                other => {
                    let error = outcome
                        .and_then(|outcome| outcome.pointer("/failure/message"))
                        .and_then(Value::as_str)
                        .unwrap_or(other)
                        .to_string();
                    vec![ProgressEvent::StageFailed {
                        node_id,
                        name: label,
                        error,
                    }]
                }
            }
        }
        "retry.scheduled" => {
            let Some(derived) = view.derived() else {
                return Vec::new();
            };
            let attempt = derived
                .get("next_attempt")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let delay_ms = derived
                .pointer("/base_delay/secs")
                .and_then(Value::as_u64)
                .map(|secs| secs.saturating_mul(1000))
                .or_else(|| derived.get("base_delay").and_then(Value::as_u64))
                .unwrap_or(0);
            vec![ProgressEvent::StageRetrying {
                name: label,
                attempt,
                max_attempts: attempt,
                delay_ms,
            }]
        }
        "fork.started" => vec![ProgressEvent::ParallelStarted],
        "branch.completed" => {
            let Some(result) = view.derived().and_then(|derived| derived.get("result")) else {
                return Vec::new();
            };
            let branch = result
                .pointer("/node/name")
                .and_then(Value::as_str)
                .unwrap_or(&node_id)
                .to_string();
            let status = match result.get("status").and_then(Value::as_str) {
                Some("success") => StageOutcome::Succeeded,
                Some("partial_success") => StageOutcome::PartiallySucceeded,
                _ => StageOutcome::Failed {
                    retry_requested: false,
                },
            };
            vec![ProgressEvent::ParallelBranchCompleted {
                branch,
                duration_ms: elapsed.unwrap_or(0),
                status,
            }]
        }
        "fork.completed" => vec![ProgressEvent::ParallelCompleted],
        "route.applied" => {
            let Some(derived) = view.derived() else {
                return Vec::new();
            };
            let Some(to_node) = derived.pointer("/target/name").and_then(Value::as_str) else {
                return Vec::new();
            };
            let back = derived
                .get("back")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if back {
                vec![ProgressEvent::LoopRestart {
                    from_node: node_id,
                    to_node:   to_node.to_string(),
                }]
            } else {
                vec![ProgressEvent::EdgeSelected {
                    from_node: node_id,
                    to_node:   to_node.to_string(),
                    label:     None,
                    condition: derived
                        .get("transition")
                        .and_then(Value::as_str)
                        .filter(|transition| *transition != "Continue")
                        .map(str::to_lowercase),
                }]
            }
        }
        "step.progress.recorded" => envelope_progress_event(view, node_id).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// The progress line of a Pebble coding-agent envelope on a stage's
/// stream, if the terminal shows it.
fn envelope_progress_event(view: PetriItem<'_>, node_id: String) -> Option<ProgressEvent> {
    let custom = view.custom()?;
    if custom.get("kind").and_then(Value::as_str) != Some("pebble") {
        return None;
    }
    let envelope: CodingAgentEvent = serde_json::from_value(custom.get("event")?.clone()).ok()?;
    let root_session = envelope.parent_session_id.is_none();
    coding_progress_event(
        node_id,
        root_session,
        Some(view.recorded_at()),
        &envelope.event,
    )
}

/// The stage's model usage from a finished visit's metrics, when the step
/// reported one.
fn usage_of(outcome: &Value) -> Option<ProgressUsage> {
    let custom = outcome.pointer("/metrics/custom")?;
    let usage = custom
        .get("pebble.usage")
        .or_else(|| custom.get("prompt.usage"))?;
    let tokens = usage.get("tokens")?;
    Some(ProgressUsage {
        input_tokens:  tokens.get("input").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: tokens.get("output").and_then(Value::as_u64).unwrap_or(0),
        cost:          usage
            .pointer("/cost/usd_micros")
            .and_then(Value::as_u64)
            .map(|micros| micros as f64 / 1_000_000.0),
    })
}

/// The progress line a platform record means, if the terminal shows it.
fn platform_progress_event(record: &Value) -> Option<ProgressEvent> {
    match record.get("kind")?.as_str()? {
        "run.created" => Some(ProgressEvent::RunCreated {
            web_url: record
                .get("web_url")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        "run.branch" => Some(ProgressEvent::WorkflowStarted {
            worktree_dir: None,
            base_branch:  record
                .get("run_branch")
                .and_then(Value::as_str)
                .map(str::to_string),
            base_sha:     record
                .get("base_sha")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        "run.notice" => Some(ProgressEvent::RunNotice {
            level:   match record.get("level").and_then(Value::as_str) {
                Some("warn") => RunNoticeLevel::Warn,
                Some("error") => RunNoticeLevel::Error,
                _ => RunNoticeLevel::Info,
            },
            code:    record
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            message: record
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "pull_request.created" => Some(ProgressEvent::PullRequestCreated {
            pr_url: record
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            draft:  record
                .get("draft")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        _ => None,
    }
}
