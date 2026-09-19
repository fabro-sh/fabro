//! The server's own reader of every run's stream.
//!
//! The Petri projector commits a run's stream (Petri's events and Fabro's
//! platform records, one `stream_seq` each) and signals after each pass.
//! This follower reads what each pass committed and hands it to the
//! server's in-process consumers: the in-memory run map the scheduler and
//! the control handlers read, and the global broadcast the `/attach`
//! stream, the Slack service and any other subscriber take their items
//! from.
//!
//! A run is followed from the moment the server launches it
//! ([`StreamFollower::follow`]): the cursor starts at the stream's head
//! then, so nothing the run recorded before this process took charge of it
//! is replayed into the live state. A run the follower first sees by its
//! signal alone is followed from its head at that moment.

use std::collections::HashMap;
use std::sync::Arc;

use fabro_store::platform_records::PlatformRecord;
use fabro_types::{RunId, RunStatus, RunStreamItem};
use tokio::sync::Mutex;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tracing::warn;

use super::{AppState, apply_lifecycle_to_managed_run, reconcile_live_interview_state};

/// How many stream items one read takes.
const BATCH_LIMIT: usize = 256;

/// The follower's cursors: the last `stream_seq` seen per run.
#[derive(Default)]
pub(crate) struct StreamFollower {
    cursors: Mutex<HashMap<RunId, u64>>,
}

/// Follow `run_id` from the stream's current head.
pub(crate) async fn follow_run(state: &AppState, run_id: RunId) {
    let head = match state.petri_projector.stream_head(run_id).await {
        Ok(head) => head.unwrap_or(0),
        Err(err) => {
            warn!(run_id = %run_id, error = %err, "the run's stream head could not be read; following from its start");
            0
        }
    };
    state
        .stream_follower
        .cursors
        .lock()
        .await
        .entry(run_id)
        .or_insert(head);
}

/// Start the follower over the projector's signals.
pub(crate) fn spawn_stream_follower(state: Arc<AppState>) -> JoinHandle<()> {
    let mut signals = state.petri_projector.subscribe();
    tokio::spawn(async move {
        loop {
            let run_id = match signals.recv().await {
                Ok(run_id) => run_id,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            if state.is_shutting_down() {
                break;
            }
            catch_up(&state, run_id).await;
        }
    })
}

/// Read what the run's stream holds past the follower's cursor and hand it
/// to the live state and the global broadcast.
async fn catch_up(state: &Arc<AppState>, run_id: RunId) {
    let mut cursor = {
        let mut cursors = state.stream_follower.cursors.lock().await;
        if let Some(cursor) = cursors.get(&run_id) {
            *cursor
        } else {
            let head = match state.petri_projector.stream_head(run_id).await {
                Ok(head) => head.unwrap_or(0),
                Err(_) => return,
            };
            cursors.insert(run_id, head);
            head
        }
    };
    let mut saw_items = false;
    loop {
        let items = match state
            .petri_projector
            .stream_after(run_id, cursor, BATCH_LIMIT)
            .await
        {
            Ok(items) => items,
            Err(err) => {
                warn!(run_id = %run_id, error = %err, "the run's stream could not be read");
                return;
            }
        };
        let drained = items.len() < BATCH_LIMIT;
        for item in items {
            cursor = item.stream_seq;
            saw_items = true;
            fold_into_live_state(state, run_id, &item);
            let _ = state.global_event_tx.send(item);
        }
        if drained {
            break;
        }
    }
    state
        .stream_follower
        .cursors
        .lock()
        .await
        .insert(run_id, cursor);
    if saw_items {
        sync_live_status_from_projection(state, run_id).await;
    }
}

/// One item into the in-memory run: its lifecycle records fold into the
/// live status; a closed question releases its answer claim.
fn fold_into_live_state(state: &AppState, run_id: RunId, item: &RunStreamItem) {
    if let Some(PlatformRecord::RunLifecycle(record)) = super::platform_record_of(item) {
        apply_lifecycle_to_managed_run(state, run_id, &record);
    }
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        reconcile_live_interview_state(managed_run, item);
    }
}

/// The blocked and paused substates of a live run come from Petri's own
/// records (a pending question, a held admission), which the projection
/// folds; the live status follows the projection there, and nowhere else.
async fn sync_live_status_from_projection(state: &AppState, run_id: RunId) {
    let Ok(Some(projection)) = state
        .stores
        .run_summaries
        .load_petri_projection(&run_id)
        .await
    else {
        return;
    };
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    let Some(managed_run) = runs.get_mut(&run_id) else {
        return;
    };
    let live = matches!(
        managed_run.status,
        RunStatus::Running | RunStatus::Blocked { .. } | RunStatus::Paused { .. }
    );
    let projected_live = matches!(
        projection.status,
        RunStatus::Running | RunStatus::Blocked { .. } | RunStatus::Paused { .. }
    );
    if live && projected_live && managed_run.status != projection.status {
        managed_run.status = projection.status;
    }
}
