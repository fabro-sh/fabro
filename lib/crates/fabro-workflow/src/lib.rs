#![cfg_attr(
    test,
    allow(
        clippy::absolute_paths,
        clippy::get_unwrap,
        clippy::large_futures,
        clippy::needless_borrows_for_generic_args,
        clippy::option_option,
        clippy::ptr_as_ptr,
        clippy::ref_as_ptr,
        clippy::cast_ptr_alignment,
        clippy::uninlined_format_args,
        clippy::unnecessary_literal_bound,
        reason = "Test-only workflow helpers favor explicit fixtures over pedantic style lints."
    )
)]

use std::collections::HashMap;
use std::sync::Arc;

use fabro_store::EventEnvelope;
use fabro_types::{EventBody, StageId};

/// Callback invoked when a workflow node starts executing.
pub type OnNodeCallback = Option<Arc<dyn Fn(&str) + Send + Sync>>;

/// Convert a Duration's milliseconds to u64, saturating on overflow.
pub(crate) fn millis_u64(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Extract the timing breakdown from a `stage.completed` / `stage.failed`
/// event body, or `None` for any other variant.
fn stage_completion_timing(body: &EventBody) -> Option<fabro_types::StageTiming> {
    match body {
        EventBody::StageCompleted(props) => Some(props.timing),
        EventBody::StageFailed(props) => Some(props.timing),
        _ => None,
    }
}

/// Extract per-stage (node_id, visit) timing from `stage.completed` /
/// `stage.failed` events. Keys on the full [`StageId`] so multi-visit stages
/// (e.g. a looped `verify` node) keep distinct timings.
///
/// This is the canonical primitive; [`total_stage_timing_by_node`] and
/// [`latest_stage_timing_by_node`] are explicit rollups built on top of it.
pub fn extract_stage_timings_by_stage_id(
    events: &[EventEnvelope],
) -> HashMap<StageId, fabro_types::StageTiming> {
    let mut timings = HashMap::new();
    for envelope in events {
        let Some(timing) = stage_completion_timing(&envelope.event.body) else {
            continue;
        };
        let Some(stage_id) = envelope.event.stage_id.as_ref() else {
            continue;
        };
        timings.insert(stage_id.clone(), timing);
    }
    timings
}

/// Sum of timing in each node across every visit. Use for billing/usage
/// where a retried node should count its full time. `wall_time_ms`,
/// `inference_time_ms`, `tool_time_ms`, and `active_time_ms` are all summed
/// per node.
pub fn total_stage_timing_by_node(
    events: &[EventEnvelope],
) -> HashMap<String, fabro_types::StageTiming> {
    let mut totals: HashMap<String, fabro_types::StageTiming> = HashMap::new();
    for (stage_id, timing) in extract_stage_timings_by_stage_id(events) {
        let entry = totals.entry(stage_id.node_id().to_string()).or_default();
        *entry = entry.saturating_add(&timing);
    }
    totals
}

/// Timing of each node's most recent visit (the highest visit number). Use
/// for run summaries where the table shows one row per node and "the last
/// attempt" is the right representative.
pub fn latest_stage_timing_by_node(
    events: &[EventEnvelope],
) -> HashMap<String, fabro_types::StageTiming> {
    let mut entries: Vec<(StageId, fabro_types::StageTiming)> =
        extract_stage_timings_by_stage_id(events)
            .into_iter()
            .collect();
    entries.sort_by_key(|(stage_id, _)| stage_id.visit());
    let mut latest = HashMap::new();
    for (stage_id, timing) in entries {
        latest.insert(stage_id.node_id().to_string(), timing);
    }
    latest
}

#[cfg(test)]
mod duration_tests {
    use chrono::{TimeZone, Utc};
    use fabro_store::EventEnvelope;
    use fabro_types::run_event::{StageCompletedProps, StageFailedProps};
    use fabro_types::{EventBody, RunEvent, StageId, StageOutcome, StageTiming, fixtures};

    use super::{
        extract_stage_timings_by_stage_id, latest_stage_timing_by_node, total_stage_timing_by_node,
    };

    fn completed_event(seq: u32, node: &str, visit: u32, wall_time_ms: u64) -> EventEnvelope {
        let event = RunEvent {
            id:                 format!("evt_{seq}"),
            ts:                 Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            run_id:             fixtures::RUN_1,
            node_id:            Some(node.to_string()),
            node_label:         None,
            stage_id:           Some(StageId::new(node, visit)),
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::StageCompleted(StageCompletedProps {
                index: 0,
                timing: StageTiming::wall_only(wall_time_ms),
                status: StageOutcome::Succeeded,
                preferred_label: None,
                suggested_next_ids: vec![],
                billing: None,
                failure: None,
                notes: None,
                files_touched: vec![],
                context_updates: None,
                jump_to_node: None,
                context_values: None,
                node_visits: None,
                loop_failure_signatures: None,
                restart_failure_signatures: None,
                response: None,
                attempt: 1,
                max_attempts: 1,
            }),
        };
        EventEnvelope { seq, event }
    }

    fn failed_event(seq: u32, node: &str, visit: u32, wall_time_ms: u64) -> EventEnvelope {
        let event = RunEvent {
            id:                 format!("evt_{seq}"),
            ts:                 Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            run_id:             fixtures::RUN_1,
            node_id:            Some(node.to_string()),
            node_label:         None,
            stage_id:           Some(StageId::new(node, visit)),
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::StageFailed(StageFailedProps {
                index:      0,
                failure:    None,
                will_retry: true,
                timing:     StageTiming::wall_only(wall_time_ms),
                billing:    None,
            }),
        };
        EventEnvelope { seq, event }
    }

    #[test]
    fn extract_keys_timings_by_full_stage_id() {
        let events = vec![
            completed_event(1, "verify", 1, 100),
            completed_event(2, "verify", 2, 200),
        ];
        let timings = extract_stage_timings_by_stage_id(&events);
        assert_eq!(
            timings
                .get(&StageId::new("verify", 1))
                .map(|t| t.wall_time_ms),
            Some(100)
        );
        assert_eq!(
            timings
                .get(&StageId::new("verify", 2))
                .map(|t| t.wall_time_ms),
            Some(200)
        );
    }

    #[test]
    fn total_sums_wall_time_across_visits_per_node() {
        let events = vec![
            completed_event(1, "verify", 1, 100),
            completed_event(2, "verify", 2, 200),
            completed_event(3, "build", 1, 50),
        ];
        let totals = total_stage_timing_by_node(&events);
        assert_eq!(totals.get("verify").map(|t| t.wall_time_ms), Some(300));
        assert_eq!(totals.get("build").map(|t| t.wall_time_ms), Some(50));
    }

    #[test]
    fn latest_picks_highest_visit_regardless_of_input_order() {
        // Visit 2 appears in the events vector before visit 1; the result
        // must still reflect visit 2's timing (the latest visit).
        let events = vec![
            completed_event(1, "verify", 2, 999),
            completed_event(2, "verify", 1, 100),
        ];
        let latest = latest_stage_timing_by_node(&events);
        assert_eq!(latest.get("verify").map(|t| t.wall_time_ms), Some(999));
    }

    #[test]
    fn stage_failed_timings_are_included() {
        let events = vec![failed_event(1, "verify", 1, 75)];
        let timings = extract_stage_timings_by_stage_id(&events);
        assert_eq!(
            timings
                .get(&StageId::new("verify", 1))
                .map(|t| t.wall_time_ms),
            Some(75)
        );
    }

    #[test]
    fn total_sums_active_breakdown_across_visits() {
        // Same node visited twice with different inference/tool breakdowns:
        // the rollup must add inference, tool, and active fields, not just
        // wall time. This guards against accidentally summing wall only.
        fn timed_completed(seq: u32, visit: u32, timing: StageTiming) -> EventEnvelope {
            let event = RunEvent {
                id:                 format!("evt_{seq}"),
                ts:                 Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                run_id:             fixtures::RUN_1,
                node_id:            Some("agent".to_string()),
                node_label:         None,
                stage_id:           Some(StageId::new("agent", visit)),
                parallel_group_id:  None,
                parallel_branch_id: None,
                session_id:         None,
                parent_session_id:  None,
                tool_call_id:       None,
                actor:              None,
                body:               EventBody::StageCompleted(StageCompletedProps {
                    index: 0,
                    timing,
                    status: StageOutcome::Succeeded,
                    preferred_label: None,
                    suggested_next_ids: vec![],
                    billing: None,
                    failure: None,
                    notes: None,
                    files_touched: vec![],
                    context_updates: None,
                    jump_to_node: None,
                    context_values: None,
                    node_visits: None,
                    loop_failure_signatures: None,
                    restart_failure_signatures: None,
                    response: None,
                    attempt: 1,
                    max_attempts: 1,
                }),
            };
            EventEnvelope { seq, event }
        }

        let events = vec![
            timed_completed(1, 1, StageTiming::new(1000, 600, 300)),
            timed_completed(2, 2, StageTiming::new(700, 400, 200)),
        ];
        let totals = total_stage_timing_by_node(&events);
        let agent = totals.get("agent").copied().unwrap();
        assert_eq!(agent.wall_time_ms, 1700);
        assert_eq!(agent.inference_time_ms, 1000);
        assert_eq!(agent.tool_time_ms, 500);
        assert_eq!(agent.active_time_ms, 1500);
    }
}

#[doc(hidden)]
pub mod artifact;
pub mod artifact_snapshot;
pub mod artifact_upload;
pub mod billing_rollup;
pub mod command_log;
pub(crate) mod condition;
pub mod context;
pub mod devcontainer_bridge;
pub mod error;
pub mod event;
pub mod file_resolver;
pub mod git;
pub mod github_token_source;
pub(crate) mod graph;
pub mod handler;
mod hook_context;
mod interview_runtime;
#[allow(
    dead_code,
    reason = "The lifecycle module remains crate-visible for tests and pending integrations."
)]
pub(crate) mod lifecycle;
pub(crate) mod node_handler;
pub mod operations;
pub mod outcome;
pub mod pipeline;
pub mod pull_request;
pub mod records;
mod retry;
pub mod run_control;
pub(crate) mod run_dir;
pub mod run_lookup;

pub use billing_rollup::{
    ProjectionBillingByModel, ProjectionBillingRollup, ProjectionBillingStage,
    billing_rollup_from_projection,
};
pub use error::{Error, FailureCategory, FailureSignature, FailureSignatureExt, Result};
pub use fabro_types::ManifestPath;
pub use steering_hub::{PairControlError, SteeringHub};
pub mod run_materialization;
pub(crate) mod run_metadata;
pub mod run_options;
pub mod run_status;
pub mod runtime_store;
pub mod sandbox_git;
pub(crate) mod sandbox_git_runtime;
pub mod services;
mod stage_scope;
pub mod static_reference;
pub mod steering_hub;
#[doc(hidden)]
pub mod test_support;
#[doc(hidden)]
pub mod transforms;
pub mod workflow_bundle;
