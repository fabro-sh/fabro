use fabro_store::Database;
use fabro_types::{ActorRef, RunId, RunStatus};

use crate::error::Error;
use crate::event::{self, Event};

/// Outcome of an `archive` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveOutcome {
    /// Event was appended; projection transitions to `Archived`.
    Archived { prior_status: RunStatus },
    /// Run was already archived; no event emitted.
    AlreadyArchived,
}

/// Outcome of an `unarchive` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnarchiveOutcome {
    /// Event was appended; projection transitions back to `restored_status`.
    Unarchived { restored_status: RunStatus },
    /// Run was terminal but not archived; no event emitted. Symmetric with
    /// `ArchiveOutcome::AlreadyArchived`.
    NotArchived { status: RunStatus },
}

/// Archive a terminal run. Idempotent if already archived.
pub async fn archive(
    store: &Database,
    run_id: &RunId,
    actor: Option<ActorRef>,
) -> Result<ArchiveOutcome, Error> {
    let run_store = store
        .open_run(run_id)
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    let projection = run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    let current = projection
        .status
        .as_ref()
        .map(|record| record.status)
        .ok_or_else(|| {
            Error::Precondition(format!("run {run_id} has no status; cannot archive"))
        })?;

    if current == RunStatus::Archived {
        return Ok(ArchiveOutcome::AlreadyArchived);
    }

    if !matches!(
        current,
        RunStatus::Succeeded | RunStatus::Failed | RunStatus::Dead
    ) {
        return Err(Error::Precondition(format!(
            "run {run_id} must be terminal (succeeded, failed, or dead) to archive; \
             current status is {current}"
        )));
    }

    event::append_event(&run_store, run_id, &Event::RunArchived { actor })
        .await
        .map_err(|err| Error::engine(err.to_string()))?;

    Ok(ArchiveOutcome::Archived {
        prior_status: current,
    })
}

/// Unarchive a previously archived run, restoring its prior terminal status.
/// Idempotent on terminal-but-not-archived runs (returns `NotArchived` without
/// emitting an event).
pub async fn unarchive(
    store: &Database,
    run_id: &RunId,
    actor: Option<ActorRef>,
) -> Result<UnarchiveOutcome, Error> {
    let run_store = store
        .open_run(run_id)
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    let projection = run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    let current = projection
        .status
        .as_ref()
        .map(|record| record.status)
        .ok_or_else(|| {
            Error::Precondition(format!("run {run_id} has no status; cannot unarchive"))
        })?;

    if current == RunStatus::Archived {
        let restored = projection.prior_status.ok_or_else(|| {
            Error::engine(format!(
                "run {run_id} is archived but prior_status is missing; projection is corrupt"
            ))
        })?;
        event::append_event(&run_store, run_id, &Event::RunUnarchived {
            actor,
            restored_status: restored,
        })
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
        return Ok(UnarchiveOutcome::Unarchived {
            restored_status: restored,
        });
    }

    if matches!(
        current,
        RunStatus::Succeeded | RunStatus::Failed | RunStatus::Dead
    ) {
        return Ok(UnarchiveOutcome::NotArchived { status: current });
    }

    Err(Error::Precondition(format!(
        "run {run_id} is not archived (status: {current}); nothing to unarchive"
    )))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_store::Database;
    use fabro_types::{RunId, fixtures};
    use object_store::memory::InMemory;

    use super::*;

    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    async fn seed_succeeded(store: &Database, run_id: &RunId) {
        let run_store = store.create_run(run_id).await.unwrap();
        event::append_event(&run_store, run_id, &Event::WorkflowRunCompleted {
            duration_ms:          10,
            artifact_count:       0,
            status:               "success".to_string(),
            reason:               None,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            billing:              None,
        })
        .await
        .unwrap();
    }

    async fn seed_failed(store: &Database, run_id: &RunId) {
        let run_store = store.create_run(run_id).await.unwrap();
        event::append_event(&run_store, run_id, &Event::WorkflowRunFailed {
            error:          crate::error::Error::engine("boom"),
            duration_ms:    10,
            reason:         None,
            git_commit_sha: None,
        })
        .await
        .unwrap();
    }

    async fn seed_running(store: &Database, run_id: &RunId) {
        let run_store = store.create_run(run_id).await.unwrap();
        event::append_event(&run_store, run_id, &Event::RunRunning { reason: None })
            .await
            .unwrap();
    }

    async fn current_status(store: &Database, run_id: &RunId) -> RunStatus {
        let run_store = store.open_run_reader(run_id).await.unwrap();
        run_store.state().await.unwrap().status.unwrap().status
    }

    async fn event_count(store: &Database, run_id: &RunId) -> usize {
        let run_store = store.open_run_reader(run_id).await.unwrap();
        run_store.list_events().await.unwrap().len()
    }

    #[tokio::test]
    async fn archive_on_succeeded_emits_event_and_transitions_to_archived() {
        let store = memory_store();
        let run_id = fixtures::RUN_1;
        seed_succeeded(&store, &run_id).await;

        let outcome = archive(&store, &run_id, None).await.unwrap();
        assert_eq!(outcome, ArchiveOutcome::Archived {
            prior_status: RunStatus::Succeeded,
        });
        assert_eq!(current_status(&store, &run_id).await, RunStatus::Archived);

        let projection = store
            .open_run_reader(&run_id)
            .await
            .unwrap()
            .state()
            .await
            .unwrap();
        assert_eq!(projection.prior_status, Some(RunStatus::Succeeded));
    }

    #[tokio::test]
    async fn archive_on_failed_captures_failed_as_prior_status() {
        let store = memory_store();
        let run_id = fixtures::RUN_2;
        seed_failed(&store, &run_id).await;

        let outcome = archive(&store, &run_id, None).await.unwrap();
        assert_eq!(outcome, ArchiveOutcome::Archived {
            prior_status: RunStatus::Failed,
        });
        assert_eq!(current_status(&store, &run_id).await, RunStatus::Archived);
    }

    #[tokio::test]
    async fn archive_on_already_archived_is_idempotent_and_emits_no_event() {
        let store = memory_store();
        let run_id = fixtures::RUN_1;
        seed_succeeded(&store, &run_id).await;
        archive(&store, &run_id, None).await.unwrap();

        let events_before = event_count(&store, &run_id).await;
        let outcome = archive(&store, &run_id, None).await.unwrap();
        let events_after = event_count(&store, &run_id).await;

        assert_eq!(outcome, ArchiveOutcome::AlreadyArchived);
        assert_eq!(events_before, events_after);
    }

    #[tokio::test]
    async fn archive_on_running_rejects_with_precondition_error() {
        let store = memory_store();
        let run_id = fixtures::RUN_1;
        seed_running(&store, &run_id).await;

        let err = archive(&store, &run_id, None).await.unwrap_err();
        let Error::Precondition(message) = err else {
            panic!("expected Precondition, got {err:?}");
        };
        assert!(
            message.contains("must be terminal"),
            "message should explain terminal requirement, got: {message}"
        );
    }

    #[tokio::test]
    async fn unarchive_restores_succeeded_and_clears_prior_status() {
        let store = memory_store();
        let run_id = fixtures::RUN_1;
        seed_succeeded(&store, &run_id).await;
        archive(&store, &run_id, None).await.unwrap();

        let outcome = unarchive(&store, &run_id, None).await.unwrap();
        assert_eq!(outcome, UnarchiveOutcome::Unarchived {
            restored_status: RunStatus::Succeeded,
        });
        let projection = store
            .open_run_reader(&run_id)
            .await
            .unwrap()
            .state()
            .await
            .unwrap();
        assert_eq!(
            projection.status.as_ref().map(|r| r.status),
            Some(RunStatus::Succeeded)
        );
        assert_eq!(projection.prior_status, None);
    }

    #[tokio::test]
    async fn unarchive_restores_failed_when_prior_was_failed() {
        let store = memory_store();
        let run_id = fixtures::RUN_2;
        seed_failed(&store, &run_id).await;
        archive(&store, &run_id, None).await.unwrap();

        let outcome = unarchive(&store, &run_id, None).await.unwrap();
        assert_eq!(outcome, UnarchiveOutcome::Unarchived {
            restored_status: RunStatus::Failed,
        });
        assert_eq!(current_status(&store, &run_id).await, RunStatus::Failed);
    }

    #[tokio::test]
    async fn unarchive_on_terminal_non_archived_run_is_idempotent_no_op() {
        let store = memory_store();
        let run_id = fixtures::RUN_1;
        seed_succeeded(&store, &run_id).await;

        let events_before = event_count(&store, &run_id).await;
        let outcome = unarchive(&store, &run_id, None).await.unwrap();
        let events_after = event_count(&store, &run_id).await;

        assert_eq!(outcome, UnarchiveOutcome::NotArchived {
            status: RunStatus::Succeeded,
        });
        assert_eq!(events_before, events_after);
    }

    #[tokio::test]
    async fn unarchive_on_running_rejects_with_precondition_error() {
        let store = memory_store();
        let run_id = fixtures::RUN_1;
        seed_running(&store, &run_id).await;

        let err = unarchive(&store, &run_id, None).await.unwrap_err();
        let Error::Precondition(message) = err else {
            panic!("expected Precondition, got {err:?}");
        };
        assert!(
            message.contains("not archived"),
            "message should explain run is not archived, got: {message}"
        );
    }

    #[tokio::test]
    async fn archive_unarchive_archive_cycle_produces_three_events() {
        let store = memory_store();
        let run_id = fixtures::RUN_1;
        seed_succeeded(&store, &run_id).await;

        let events_before = event_count(&store, &run_id).await;
        archive(&store, &run_id, None).await.unwrap();
        unarchive(&store, &run_id, None).await.unwrap();
        archive(&store, &run_id, None).await.unwrap();
        let events_after = event_count(&store, &run_id).await;

        assert_eq!(events_after - events_before, 3);
        assert_eq!(current_status(&store, &run_id).await, RunStatus::Archived);
    }
}
