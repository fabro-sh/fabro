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

pub fn extract_stage_durations_from_events(events: &[EventEnvelope]) -> HashMap<String, u64> {
    let mut durations = HashMap::new();
    for envelope in events {
        let event = &envelope.event;
        let event_name = event.event_name();
        if event_name != "stage.completed" && event_name != "stage.failed" {
            continue;
        }
        let Some(node_id) = event.node_id.as_deref() else {
            continue;
        };
        let Some(duration_ms) = event
            .properties()
            .ok()
            .and_then(|properties| properties.get("duration_ms").cloned())
            .and_then(|duration| duration.as_u64())
        else {
            continue;
        };
        durations.insert(node_id.to_string(), duration_ms);
    }
    durations
}

#[doc(hidden)]
pub mod artifact;
pub mod artifact_snapshot;
pub mod artifact_upload;
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
#[doc(hidden)]
pub mod test_support;
#[doc(hidden)]
pub mod transforms;
pub mod workflow_bundle;
