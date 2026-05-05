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

use fabro_retro::retro::CompletedStage;
use fabro_store::EventEnvelope;
use fabro_types::{EventBody, StageId};

/// Callback invoked when a workflow node starts executing.
pub type OnNodeCallback = Option<Arc<dyn Fn(&str) + Send + Sync>>;

/// Convert a Duration's milliseconds to u64, saturating on overflow.
pub(crate) fn millis_u64(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Build `Vec<CompletedStage>` from a `Checkpoint`, mapping workflow-engine
/// types into the flat struct expected by `fabro_retro::retro::derive_retro`.
pub fn build_completed_stages(cp: &records::Checkpoint, run_failed: bool) -> Vec<CompletedStage> {
    use outcome::OutcomeExt;

    let mut stages = Vec::new();
    let mut any_stage_failed = false;

    for node_id in &cp.completed_nodes {
        let outcome = cp.node_outcomes.get(node_id);
        let retries = cp.node_retries.get(node_id).copied().unwrap_or(0);

        let status = outcome.map_or_else(|| "unknown".to_string(), |o| o.status.to_string());

        let succeeded = outcome.is_some_and(|o| o.status.is_successful());
        let failed = outcome.is_some_and(|o| o.status.is_failure());
        if failed {
            any_stage_failed = true;
        }

        stages.push(CompletedStage {
            node_id: node_id.clone(),
            status,
            succeeded,
            failed,
            retries,
            billing_usd_micros: outcome
                .and_then(|o| o.usage.as_ref())
                .and_then(|usage| usage.total_usd_micros),
            notes: outcome.and_then(|o| o.notes.clone()),
            failure_reason: outcome.and_then(|o| o.failure_reason().map(String::from)),
            files_touched: outcome.map(|o| o.files_touched.clone()).unwrap_or_default(),
        });
    }

    // If run failed with an error not captured in stages, mark the last stage
    if run_failed && !any_stage_failed {
        if let Some(last) = stages.last_mut() {
            last.failed = true;
        } else {
            stages.push(CompletedStage {
                node_id:            "unknown".to_string(),
                status:             "failed".to_string(),
                succeeded:          false,
                failed:             true,
                retries:            0,
                billing_usd_micros: None,
                notes:              None,
                failure_reason:     None,
                files_touched:      vec![],
            });
        }
    }

    stages
}

/// Extract the `duration_ms` from a `stage.completed` / `stage.failed`
/// event body, or `None` for any other variant.
fn stage_completion_duration_ms(body: &EventBody) -> Option<u64> {
    match body {
        EventBody::StageCompleted(props) => Some(props.duration_ms),
        EventBody::StageFailed(props) => Some(props.duration_ms),
        _ => None,
    }
}

/// Extract per-stage (node_id, visit) durations from `stage.completed` /
/// `stage.failed` events. Keys on the full [`StageId`] so multi-visit stages
/// (e.g. a looped `verify` node) keep distinct durations.
///
/// This is the canonical primitive; [`total_stage_duration_by_node`] and
/// [`latest_stage_duration_by_node`] are explicit rollups built on top of it.
pub fn extract_stage_durations_by_stage_id(events: &[EventEnvelope]) -> HashMap<StageId, u64> {
    let mut durations = HashMap::new();
    for envelope in events {
        let Some(duration_ms) = stage_completion_duration_ms(&envelope.event.body) else {
            continue;
        };
        let Some(stage_id) = envelope.event.stage_id.as_ref() else {
            continue;
        };
        durations.insert(stage_id.clone(), duration_ms);
    }
    durations
}

/// Total duration spent in each node, summed across every visit. Use for
/// billing/usage where a retried node should count its full time.
pub fn total_stage_duration_by_node(events: &[EventEnvelope]) -> HashMap<String, u64> {
    let mut totals: HashMap<String, u64> = HashMap::new();
    for (stage_id, duration_ms) in extract_stage_durations_by_stage_id(events) {
        *totals.entry(stage_id.node_id().to_string()).or_default() += duration_ms;
    }
    totals
}

/// Duration of each node's most recent visit (the highest visit number). Use
/// for run summaries and retros where the table shows one row per node and
/// "the last attempt" is the right representative.
pub fn latest_stage_duration_by_node(events: &[EventEnvelope]) -> HashMap<String, u64> {
    let mut entries: Vec<(StageId, u64)> = extract_stage_durations_by_stage_id(events)
        .into_iter()
        .collect();
    entries.sort_by_key(|(stage_id, _)| stage_id.visit());
    let mut latest = HashMap::new();
    for (stage_id, duration_ms) in entries {
        latest.insert(stage_id.node_id().to_string(), duration_ms);
    }
    latest
}

#[cfg(test)]
mod duration_tests {
    use chrono::{TimeZone, Utc};
    use fabro_store::EventEnvelope;
    use fabro_types::run_event::{StageCompletedProps, StageFailedProps};
    use fabro_types::{EventBody, RunEvent, StageId, StageOutcome, fixtures};

    use super::{
        extract_stage_durations_by_stage_id, latest_stage_duration_by_node,
        total_stage_duration_by_node,
    };

    fn completed_event(seq: u32, node: &str, visit: u32, duration_ms: u64) -> EventEnvelope {
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
                duration_ms,
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

    fn failed_event(seq: u32, node: &str, visit: u32, duration_ms: u64) -> EventEnvelope {
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
                index: 0,
                failure: None,
                will_retry: true,
                duration_ms,
                billing: None,
            }),
        };
        EventEnvelope { seq, event }
    }

    #[test]
    fn extract_keys_durations_by_full_stage_id() {
        let events = vec![
            completed_event(1, "verify", 1, 100),
            completed_event(2, "verify", 2, 200),
        ];
        let durations = extract_stage_durations_by_stage_id(&events);
        assert_eq!(
            durations.get(&StageId::new("verify", 1)).copied(),
            Some(100)
        );
        assert_eq!(
            durations.get(&StageId::new("verify", 2)).copied(),
            Some(200)
        );
    }

    #[test]
    fn total_sums_across_visits_per_node() {
        let events = vec![
            completed_event(1, "verify", 1, 100),
            completed_event(2, "verify", 2, 200),
            completed_event(3, "build", 1, 50),
        ];
        let totals = total_stage_duration_by_node(&events);
        assert_eq!(totals.get("verify").copied(), Some(300));
        assert_eq!(totals.get("build").copied(), Some(50));
    }

    #[test]
    fn latest_picks_highest_visit_regardless_of_input_order() {
        // Visit 2 appears in the events vector before visit 1; the result
        // must still reflect visit 2's duration (the latest visit).
        let events = vec![
            completed_event(1, "verify", 2, 999),
            completed_event(2, "verify", 1, 100),
        ];
        let latest = latest_stage_duration_by_node(&events);
        assert_eq!(latest.get("verify").copied(), Some(999));
    }

    #[test]
    fn stage_failed_durations_are_included() {
        let events = vec![failed_event(1, "verify", 1, 75)];
        let durations = extract_stage_durations_by_stage_id(&events);
        assert_eq!(durations.get(&StageId::new("verify", 1)).copied(), Some(75));
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
pub(crate) mod graph;
pub mod handler;
mod hook_context;
#[allow(
    dead_code,
    reason = "The lifecycle module remains crate-visible for tests and pending integrations."
)]
pub(crate) mod lifecycle;
mod manifest_path;
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
pub use manifest_path::ManifestPath;
pub mod run_materialization;
pub(crate) mod run_metadata;
pub mod run_options;
pub mod run_status;
pub mod runtime_store;
pub mod sandbox_git;
pub(crate) mod sandbox_git_runtime;
pub mod services;
mod stage_scope;
#[doc(hidden)]
pub mod test_support;
#[doc(hidden)]
pub mod transforms;
pub mod workflow_bundle;
