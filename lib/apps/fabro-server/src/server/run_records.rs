//! Fabro's own facts about a run, written and read by the server.
//!
//! A run's lifecycle before, beside and after the engine (its queue, an
//! approval, a control request, the terminal status Fabro reports), its
//! title and parent, its pull request and its notices are platform records
//! (`fabro_store::platform_records`), appended here and folded into the
//! run's projection by the Petri projector. Every append wakes the
//! projector; a caller that reads the run back right after waits for that
//! pass, so what it reads holds what it wrote.

use std::sync::Arc;

use anyhow::Context as _;
use axum::http::StatusCode;
use fabro_store::RunProjection;
use fabro_store::platform_records::{
    PlatformRecord, RunLifecycleKind, RunLifecycleRecord, StoredPlatformRecord,
};
use fabro_types::{FailureReason, RunId, RunStatus, SuccessReason};

use super::AppState;
use crate::error::ApiError;

/// Append one record for the run, wake its projector and wait for the
/// pass that folds it: what the caller reads next holds the record.
pub(crate) async fn append(
    state: &AppState,
    run_id: RunId,
    record: PlatformRecord,
) -> anyhow::Result<StoredPlatformRecord> {
    let summaries = &state.stores.run_summaries;
    let stored = summaries
        .platform_records()
        .append(&run_id, &record, None)
        .await
        .with_context(|| format!("appending a {} record for run {run_id}", record.kind()))?;
    summaries.notify_platform_record(run_id);
    state.petri_projector.settle(run_id).await;
    Ok(stored)
}

/// Append one lifecycle transition for the run.
pub(crate) async fn lifecycle(
    state: &AppState,
    run_id: RunId,
    record: RunLifecycleRecord,
) -> anyhow::Result<StoredPlatformRecord> {
    append(state, run_id, PlatformRecord::RunLifecycle(record)).await
}

/// A transition that leads to `status`.
#[must_use]
pub(crate) fn transition(kind: RunLifecycleKind, status: RunStatus) -> RunLifecycleRecord {
    RunLifecycleRecord::new(kind).with_status(status)
}

/// The run failed for `reason`, with `message` as the failure's detail.
#[must_use]
pub(crate) fn failed(reason: FailureReason, message: impl Into<String>) -> RunLifecycleRecord {
    let mut record = transition(RunLifecycleKind::Failed, RunStatus::Failed { reason });
    record.reason = Some(message.into());
    record
}

/// The run succeeded for `reason`.
#[must_use]
pub(crate) fn succeeded(reason: SuccessReason) -> RunLifecycleRecord {
    transition(RunLifecycleKind::Succeeded, RunStatus::Succeeded { reason })
}

/// The run's projection once every committed record is folded: the read
/// that follows a write.
pub(crate) async fn projection(
    state: &AppState,
    run_id: RunId,
) -> anyhow::Result<Option<Arc<RunProjection>>> {
    state.petri_projector.settle(run_id).await;
    state
        .stores
        .run_summaries
        .load_petri_projection(&run_id)
        .await
        .with_context(|| format!("loading the projection of run {run_id}"))
}

/// [`projection`], as an API handler needs it: a missing run is the
/// canonical 404, a store failure a 500.
pub(crate) async fn require_projection(
    state: &AppState,
    run_id: RunId,
) -> Result<Arc<RunProjection>, ApiError> {
    projection(state, run_id)
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
        .ok_or_else(|| ApiError::not_found("Run not found."))
}
