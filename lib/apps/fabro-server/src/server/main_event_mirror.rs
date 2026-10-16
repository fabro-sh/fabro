//! Replay selected lifecycle records into the independent global stream.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use fabro_petri::main_events;
use fabro_store::MainEventSourceKey;
use fabro_types::{RunId, RunProjection, RunStreamItem, RunStreamItemKind};
use tokio::sync::broadcast::error::RecvError;
use tokio::time;
use tracing::warn;

use super::AppState;

const BATCH_LIMIT: usize = 256;

/// Mirror a native stream item with a stable source key. The SQL stream is the
/// outbox: replay after a crash retries the append without duplication.
async fn mirror_native_item(
    state: &AppState,
    item: &RunStreamItem,
    projection: &RunProjection,
) -> anyhow::Result<()> {
    let Some(body) = main_events::from_stream_item(item, projection)? else {
        return Ok(());
    };
    state
        .stores
        .runs
        .main_events()
        .append_once(
            MainEventSourceKey::RunStream {
                run_id:  item.run_id,
                item_id: format!("{}:{}", item.kind, item.id),
            },
            body,
        )
        .await?;
    Ok(())
}

/// Reconcile all native streams on startup and periodically. Signals shorten
/// delivery latency; the scan also repairs gaps when a signal is lost or a
/// process exits between the run record commit and the main event append.
pub(crate) fn spawn_native_mirror(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut cursors = HashMap::new();
        let mut signals = state.petri_projector.subscribe();
        let mut tick = time::interval(Duration::from_secs(30));
        let shutdown = state.shutdown_token();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = tick.tick() => {
                    match state.stores.run_summaries.list_identities().await {
                        Ok(runs) => for run in runs {
                            let cursor = cursors.entry(run.id).or_insert(0);
                            if let Err(error) = reconcile_native_run(&state, run.id, cursor).await {
                                warn!(run_id = %run.id, error = %error, "Native main event mirror will retry");
                            }
                        },
                        Err(error) => warn!(error = %error, "Main event mirror could not list runs; will retry"),
                    }
                }
                signal = signals.recv() => match signal {
                    Ok(run_id) => {
                        let cursor = cursors.entry(run_id).or_insert(0);
                        if let Err(error) = reconcile_native_run(&state, run_id, cursor).await {
                            warn!(%run_id, error = %error, "Native main event mirror will retry");
                        }
                    }
                    Err(RecvError::Closed) => break,
                    Err(RecvError::Lagged(_)) => {},
                },
            }
        }
    });
}

async fn reconcile_native_run(
    state: &AppState,
    run_id: RunId,
    cursor: &mut u64,
) -> anyhow::Result<()> {
    loop {
        let items = state
            .petri_projector
            .stream_after(run_id, *cursor, BATCH_LIMIT)
            .await?;
        if items.is_empty() {
            return Ok(());
        }
        // Read after the stream page: the projection transaction that published
        // these items must be visible before terminal usage is mirrored.
        let Some(projection) = state
            .stores
            .run_summaries
            .load_petri_projection(&run_id)
            .await?
        else {
            return Ok(());
        };
        let drained = items.len() < BATCH_LIMIT;
        for item in items {
            mirror_native_item(state, &item, &projection).await?;
            *cursor = item.stream_seq;
        }
        if drained {
            return Ok(());
        }
    }
}

/// Creation is mirrored before a successful create response. The background
/// mirror uses the same source key, so either delivery can win safely.
pub(crate) async fn mirror_created(state: &AppState, run_id: RunId) -> anyhow::Result<()> {
    state.petri_projector.settle(run_id).await;
    let items = state.petri_projector.stream_after(run_id, 0, 1).await?;
    let created = items
        .first()
        .filter(|item| {
            item.kind == RunStreamItemKind::Platform && item.name() == Some("run.created")
        })
        .context("created run has no initial run.created stream item")?;
    let projection = state
        .stores
        .run_summaries
        .load_petri_projection(&run_id)
        .await?
        .context("created run has no projection")?;
    mirror_native_item(state, created, &projection).await
}

#[cfg(test)]
mod tests {
    use fabro_store::platform_records::{
        PlatformRecord, RunCreatedRecord, RunLifecycleKind, RunLifecycleRecord,
    };
    use fabro_types::{FailureReason, MainEventBody, RunStatus, test_support};
    use tokio::sync::broadcast::error::TryRecvError;

    use super::*;
    use crate::server::run_records;

    async fn create(state: &AppState) -> RunId {
        let mut spec = test_support::test_run_spec();
        spec.run_id = RunId::new();
        let run_id = spec.run_id;
        run_records::append(
            state,
            run_id,
            PlatformRecord::RunCreated(RunCreatedRecord {
                spec,
                title: Some("Original title".into()),
                parent_id: None,
                retried_from: None,
                web_url: None,
            }),
        )
        .await
        .unwrap();
        run_id
    }

    #[tokio::test]
    async fn main_event_mirror_replays_lifecycle_once_and_keeps_events_after_deletion() {
        let state = crate::test_support::test_app_state();
        let run_id = create(&state).await;
        mirror_created(&state, run_id).await.unwrap();
        let events = state.stores.runs.main_events();
        let created = events.list_from_with_limit(1, 10).await.unwrap();
        assert_eq!(created.len(), 1);
        let MainEventBody::RunCreated(created) = &created[0].event.body else {
            panic!("created");
        };
        assert_eq!(created.title.as_deref(), Some("Original title"));

        for transition in [
            RunLifecycleKind::Running,
            RunLifecycleKind::PauseRequested,
            RunLifecycleKind::Paused,
            RunLifecycleKind::Unpaused,
            RunLifecycleKind::CancelRequested,
        ] {
            run_records::lifecycle(&state, run_id, RunLifecycleRecord::new(transition))
                .await
                .unwrap();
        }
        run_records::lifecycle(
            &state,
            run_id,
            run_records::failed(FailureReason::Cancelled, "Stopped by user"),
        )
        .await
        .unwrap();
        let mut cursor = 0;
        reconcile_native_run(&state, run_id, &mut cursor)
            .await
            .unwrap();
        let mirrored = events.list_from_with_limit(1, 20).await.unwrap();
        assert_eq!(
            mirrored
                .iter()
                .map(|event| event.event.event_name())
                .collect::<Vec<_>>(),
            [
                "fabro.run.created",
                "fabro.run.running",
                "fabro.run.paused",
                "fabro.run.unpaused",
                "fabro.run.cancelled"
            ]
        );
        let MainEventBody::RunCancelled(cancelled) = &mirrored[4].event.body else {
            panic!("cancelled");
        };
        assert_eq!(cancelled.message, "Stopped by user");
        let head = state
            .petri_projector
            .stream_head(run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cursor, head);

        let mut live = events.subscribe();
        // A new process starts with no in-memory cursor, then replays its outbox.
        reconcile_native_run(&state, run_id, &mut 0).await.unwrap();
        assert_eq!(live.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(events.list_from_with_limit(1, 20).await.unwrap(), mirrored);
        state.petri_projector.delete_run(run_id).await.unwrap();
        assert_eq!(events.list_from_with_limit(1, 20).await.unwrap(), mirrored);
    }

    #[tokio::test]
    async fn main_event_mirror_retries_failed_delivery_without_advancing_cursor() {
        let state = crate::test_support::test_app_state();
        let run_id = create(&state).await;
        let pool = state.stores.run_summaries.pool();
        sqlx::raw_sql("CREATE TRIGGER fail_main_event BEFORE INSERT ON main_events BEGIN SELECT RAISE(ABORT, 'injected failure'); END;")
            .execute(&pool).await.unwrap();
        assert!(mirror_created(&state, run_id).await.is_err());
        let mut cursor = 0;
        assert!(
            reconcile_native_run(&state, run_id, &mut cursor)
                .await
                .is_err()
        );
        assert_eq!(cursor, 0);
        sqlx::query("DROP TRIGGER fail_main_event")
            .execute(&pool)
            .await
            .unwrap();
        reconcile_native_run(&state, run_id, &mut cursor)
            .await
            .unwrap();
        assert!(cursor > 0);
        mirror_created(&state, run_id).await.unwrap();
        let events = state
            .stores
            .runs
            .main_events()
            .list_from_with_limit(1, 10)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 1);
    }

    #[tokio::test]
    async fn main_event_mirror_drains_multiple_pages_without_changing_the_run_stream() {
        let state = crate::test_support::test_app_state();
        let run_id = create(&state).await;
        let records = state.stores.run_summaries.platform_records();
        for _ in 0..BATCH_LIMIT {
            records
                .append(
                    &run_id,
                    &PlatformRecord::RunLifecycle(
                        RunLifecycleRecord::new(RunLifecycleKind::Running)
                            .with_status(RunStatus::Running),
                    ),
                    None,
                )
                .await
                .unwrap();
        }
        state.stores.run_summaries.notify_platform_record(run_id);
        state.petri_projector.settle(run_id).await;
        let original = state
            .petri_projector
            .stream_after(run_id, 0, BATCH_LIMIT * 2)
            .await
            .unwrap();
        let mut cursor = 0;
        reconcile_native_run(&state, run_id, &mut cursor)
            .await
            .unwrap();
        assert_eq!(cursor, original.last().unwrap().stream_seq);
        assert_eq!(
            state
                .petri_projector
                .stream_after(run_id, 0, BATCH_LIMIT * 2)
                .await
                .unwrap(),
            original
        );
        let events = state
            .stores
            .runs
            .main_events()
            .list_from_with_limit(1, BATCH_LIMIT * 2)
            .await
            .unwrap();
        assert_eq!(events.len(), BATCH_LIMIT + 1);
    }

    #[tokio::test]
    async fn main_event_mirror_startup_reconciles_without_a_new_signal() {
        let state = crate::test_support::test_app_state();
        let run_id = create(&state).await;
        let events = state.stores.runs.main_events();
        let mut live = events.subscribe();
        spawn_native_mirror(Arc::clone(&state));
        let event = time::timeout(Duration::from_secs(5), live.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event.event.body, MainEventBody::RunCreated(_)));
        mirror_created(&state, run_id).await.unwrap();
        assert_eq!(events.list_from_with_limit(1, 10).await.unwrap().len(), 1);
        state.shutdown_token().cancel();
    }
}
