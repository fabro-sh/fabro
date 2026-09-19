//! Background processing for durably accepted pull request creations.
//!
//! `POST /runs/{id}/pull_request` records a `pull_request.creation_requested`
//! event and returns 202; this supervisor finds pending creations (including
//! after a server restart), runs them under a bounded worker pool, and
//! records a durable success or failure result.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use fabro_store::platform_records::{
    PlatformRecord, PullRequestCreatedRecord, PullRequestFailedRecord,
};
use fabro_types::{PullRequestCreation, PullRequestCreationId, RunId};
use tokio::task::{self, JoinHandle, JoinSet};
use tokio::time;
use tracing::{Instrument as _, info_span, warn};

use super::handler::pull_requests::{
    RunPrInputs, load_server_github_credentials, server_github_context,
};
use super::{AppState, pull_request, run_records};

const PULL_REQUEST_CREATION_TIMEOUT: Duration = Duration::from_mins(10);
const PULL_REQUEST_CREATION_SCAN_INTERVAL: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_PULL_REQUEST_CREATIONS: usize = 4;
const PULL_REQUEST_CREATION_QUEUE_CAPACITY: usize = MAX_CONCURRENT_PULL_REQUEST_CREATIONS * 4;
/// Stop retrying a run after this many worker attempts that could not even
/// record a durable failure (store errors). Without a cap, such a run would
/// re-run the whole attempt — including the LLM call — on every scan.
const MAX_WORKER_FAILURES_PER_RUN: u32 = 3;

pub(super) struct PendingPullRequestCreationQueue {
    capacity: usize,
    ordered:  BTreeSet<(chrono::DateTime<chrono::Utc>, RunId)>,
    run_ids:  HashSet<RunId>,
}

impl PendingPullRequestCreationQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            ordered: BTreeSet::new(),
            run_ids: HashSet::new(),
        }
    }

    fn push(&mut self, run_id: RunId, requested_at: chrono::DateTime<chrono::Utc>) -> bool {
        if self.ordered.len() >= self.capacity || !self.run_ids.insert(run_id) {
            return false;
        }
        self.ordered.insert((requested_at, run_id));
        true
    }

    fn pop(&mut self) -> Option<RunId> {
        let (_, run_id) = self.ordered.pop_first()?;
        self.run_ids.remove(&run_id);
        Some(run_id)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.ordered.len()
    }
}

impl Default for PendingPullRequestCreationQueue {
    fn default() -> Self {
        Self::new(PULL_REQUEST_CREATION_QUEUE_CAPACITY)
    }
}

impl AppState {
    pub(super) fn enqueue_pull_request_creation(
        &self,
        run_id: RunId,
        requested_at: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        self.pull_request_creation_queue
            .lock()
            .expect("pull request creation queue lock poisoned")
            .push(run_id, requested_at)
    }

    fn pop_pull_request_creation(&self) -> Option<RunId> {
        self.pull_request_creation_queue
            .lock()
            .expect("pull request creation queue lock poisoned")
            .pop()
    }
}

async fn append_pull_request_creation_failure(
    state: &AppState,
    run_id: &RunId,
    creation_id: PullRequestCreationId,
    error: String,
) -> anyhow::Result<()> {
    let still_pending = run_records::projection(state, *run_id)
        .await?
        .is_some_and(|projection| is_pending_creation(&projection, creation_id));
    if !still_pending {
        return Ok(());
    }
    run_records::append(
        state,
        *run_id,
        PlatformRecord::PullRequestFailed(PullRequestFailedRecord {
            creation_id: Some(creation_id),
            error,
        }),
    )
    .await?;
    Ok(())
}

fn is_pending_creation(
    projection: &fabro_store::RunProjection,
    creation_id: PullRequestCreationId,
) -> bool {
    projection
        .pull_request_creation
        .as_ref()
        .is_some_and(|creation| creation.id == creation_id && creation.is_pending())
}

pub(in crate::server) async fn process_pull_request_creation(
    state: Arc<AppState>,
    run_id: RunId,
) -> anyhow::Result<()> {
    let _create_guard = state.pull_request_create_locks.lock(run_id).await;
    let Some(run_state) = run_records::projection(&state, run_id).await? else {
        return Ok(());
    };
    let Some(creation) = run_state
        .pull_request_creation
        .as_ref()
        .filter(|creation| creation.is_pending())
        .cloned()
    else {
        return Ok(());
    };

    match attempt_pull_request_creation(&state, &run_id, &run_state, &creation).await? {
        Ok(()) => Ok(()),
        Err(error) => {
            append_pull_request_creation_failure(&state, &run_id, creation.id, error).await
        }
    }
}

/// One end-to-end creation attempt. The inner `Err` is a durable creation
/// failure for the caller to record; the inner `Ok` covers success and
/// shutdown-interrupted attempts (which stay pending). The outer `Err` is an
/// infrastructure failure — nothing was recorded, so the supervisor may retry.
async fn attempt_pull_request_creation(
    state: &AppState,
    run_id: &RunId,
    run_state: &fabro_store::RunProjection,
    creation: &PullRequestCreation,
) -> anyhow::Result<Result<(), String>> {
    let inputs = match RunPrInputs::extract(run_state, creation.force) {
        Ok(inputs) => inputs,
        Err(err) => return Ok(Err(err.detail().to_string())),
    };
    let creds = match load_server_github_credentials(state).await {
        Ok(creds) => creds,
        Err(err) => return Ok(Err(err.detail().to_string())),
    };
    let github = match server_github_context(state, &creds) {
        Ok(github) => github,
        Err(err) => return Ok(Err(err.detail().to_string())),
    };
    let catalog = state.catalog();
    let request = pull_request::OpenPullRequestRequest {
        github,
        origin_url: &inputs.normalized_origin,
        base_branch: inputs.base_branch,
        head_branch: inputs.run_branch,
        expected_head_sha: inputs.final_git_sha,
        goal: inputs.goal,
        diff: inputs.diff,
        model: &creation.model,
        draft: true,
        auto_merge: None,
        llm_source: Arc::clone(&state.llm_source),
        catalog,
        conclusion: Some(inputs.conclusion),
        run_state: Some(run_state),
    };
    let shutdown = state.shutdown_token();
    let result = tokio::select! {
        () = shutdown.cancelled() => return Ok(Ok(())),
        result = time::timeout(PULL_REQUEST_CREATION_TIMEOUT, pull_request::open_pull_request(request)) => result,
    };
    let created_pull_request = match result {
        Ok(Ok(created)) => created,
        Ok(Err(err)) => return Ok(Err(err)),
        Err(_) => {
            return Ok(Err(format!(
                "Pull request creation timed out after {} minutes.",
                PULL_REQUEST_CREATION_TIMEOUT.as_secs() / 60
            )));
        }
    };

    let still_pending = run_records::projection(state, *run_id)
        .await?
        .is_some_and(|projection| {
            projection.pull_request.is_none() && is_pending_creation(&projection, creation.id)
        });
    if still_pending {
        let link = &created_pull_request.link;
        run_records::append(
            state,
            *run_id,
            PlatformRecord::PullRequestCreated(PullRequestCreatedRecord {
                number:    link.number,
                owner:     link.owner.clone(),
                repo:      link.repo.clone(),
                html_url:  link.html_url(),
                head_sha:  Some(inputs.final_git_sha.to_string()),
                draft:     true,
                operation: None,
            }),
        )
        .await?;
    }
    Ok(Ok(()))
}

pub(crate) fn spawn_pull_request_creation_supervisor(state: Arc<AppState>) -> JoinHandle<()> {
    tokio::spawn(
        run_pull_request_creation_supervisor(state)
            .instrument(info_span!("pull_request_creation_supervisor")),
    )
}

pub(super) async fn recover_pending_pull_request_creations(
    state: &AppState,
    active: &HashMap<task::Id, RunId>,
    failures: &HashMap<RunId, u32>,
) -> anyhow::Result<()> {
    let candidates = state
        .stores
        .run_summaries
        .list_pull_request_creation_candidate_run_ids()
        .await?;
    let candidate_count = candidates.len();
    let mut pending = Vec::new();
    let mut load_errors = 0_usize;

    for run_id in candidates {
        if !can_dispatch(&run_id, active, failures) {
            continue;
        }
        let projection = match run_records::projection(state, run_id).await {
            Ok(Some(projection)) => projection,
            Ok(None) => continue,
            Err(error) => {
                load_errors += 1;
                warn!(%run_id, %error, "Failed to load pull request creation candidate");
                continue;
            }
        };
        let Some(creation) = projection
            .pull_request_creation
            .as_ref()
            .filter(|creation| creation.is_pending())
        else {
            continue;
        };
        pending.push((creation.requested_at, run_id));
    }

    pending.sort_unstable();
    let pending_count = pending.len();
    let enqueued = pending
        .into_iter()
        .filter(|(requested_at, run_id)| {
            state.enqueue_pull_request_creation(*run_id, *requested_at)
        })
        .count();
    tracing::debug!(
        candidate_count,
        pending_count,
        enqueued,
        load_errors,
        "Recovered pending pull request creations"
    );
    Ok(())
}

/// Whether the supervisor may hand `run_id` to a worker right now: not
/// already being processed, and not past the store-failure retry cap.
fn can_dispatch(
    run_id: &RunId,
    active: &HashMap<task::Id, RunId>,
    failures: &HashMap<RunId, u32>,
) -> bool {
    !active.values().any(|active_id| active_id == run_id)
        && failures.get(run_id).copied().unwrap_or(0) < MAX_WORKER_FAILURES_PER_RUN
}

async fn run_pull_request_creation_supervisor(state: Arc<AppState>) {
    let shutdown = state.shutdown_token();
    let mut workers = JoinSet::new();
    let mut active: HashMap<task::Id, RunId> = HashMap::new();
    let mut failures: HashMap<RunId, u32> = HashMap::new();
    let mut scan_requested = true;
    let mut scan_interval = time::interval(PULL_REQUEST_CREATION_SCAN_INTERVAL);
    scan_interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

    loop {
        if scan_requested {
            if let Err(error) =
                recover_pending_pull_request_creations(&state, &active, &failures).await
            {
                warn!(%error, "Failed to scan queued pull request creations");
            }
            scan_requested = false;
        }

        while active.len() < MAX_CONCURRENT_PULL_REQUEST_CREATIONS {
            let Some(run_id) = state.pop_pull_request_creation() else {
                break;
            };
            if !can_dispatch(&run_id, &active, &failures) {
                continue;
            }
            let handle = workers.spawn(
                process_pull_request_creation(Arc::clone(&state), run_id)
                    .instrument(info_span!("pull_request_creation", run_id = %run_id)),
            );
            active.insert(handle.id(), run_id);
        }

        if shutdown.is_cancelled() {
            break;
        }

        if workers.is_empty() {
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = state.pull_request_scheduler_notified() => {},
                _ = scan_interval.tick() => scan_requested = true,
            }
            continue;
        }

        tokio::select! {
            () = shutdown.cancelled() => break,
            () = state.pull_request_scheduler_notified() => {},
            _ = scan_interval.tick() => scan_requested = true,
            joined = workers.join_next_with_id() => {
                match joined {
                    Some(Ok((task_id, result))) => {
                        let run_id = active.remove(&task_id);
                        match (run_id, result) {
                            (Some(run_id), Ok(())) => {
                                failures.remove(&run_id);
                            }
                            (Some(run_id), Err(err)) => {
                                // Deliberately no immediate rescan: the run's
                                // creation is still pending, and re-picking it
                                // now would retry the whole attempt in a tight
                                // loop. The next interval tick retries it.
                                *failures.entry(run_id).or_default() += 1;
                                warn!(run_id = %run_id, error = %err, "Pull request creation worker failed");
                            }
                            (None, result) => {
                                warn!(?result, "Pull request creation worker finished without a tracked run id");
                            }
                        }
                    }
                    Some(Err(err)) => {
                        if let Some(run_id) = active.remove(&err.id()) {
                            *failures.entry(run_id).or_default() += 1;
                            warn!(run_id = %run_id, error = %err, "Pull request creation worker stopped unexpectedly");
                        } else {
                            warn!(error = %err, "Pull request creation worker stopped unexpectedly");
                        }
                    }
                    None => {}
                }
            }
        }
    }

    while let Some(joined) = workers.join_next().await {
        if let Err(err) = joined {
            warn!(error = %err, "Pull request creation worker stopped during shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::PendingPullRequestCreationQueue;

    fn dt(value: &str) -> DateTime<Utc> {
        value.parse().expect("test timestamp should parse")
    }

    #[test]
    fn pull_request_creation_queue_deduplicates_orders_and_bounds_work() {
        let requested_at = dt("2026-08-31T12:00:00Z");
        let earlier = requested_at - chrono::Duration::seconds(1);
        let tied_low = fabro_types::RunId::new();
        let tied_high = fabro_types::RunId::new();
        let (tied_low, tied_high) = if tied_low < tied_high {
            (tied_low, tied_high)
        } else {
            (tied_high, tied_low)
        };
        let oldest = fabro_types::RunId::new();
        let overflow = fabro_types::RunId::new();
        let mut queue = PendingPullRequestCreationQueue::new(3);

        assert!(queue.push(tied_high, requested_at));
        assert!(queue.push(oldest, earlier));
        assert!(queue.push(tied_low, requested_at));
        assert!(!queue.push(tied_high, requested_at));
        assert!(!queue.push(overflow, requested_at));
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.pop(), Some(oldest));
        assert_eq!(queue.pop(), Some(tied_low));
        assert_eq!(queue.pop(), Some(tied_high));
        assert_eq!(queue.pop(), None);

        assert!(queue.push(overflow, requested_at));
        assert_eq!(queue.pop(), Some(overflow));
    }
}
