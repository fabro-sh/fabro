//! The compact server-wide event stream, independent of any run's lifetime.
//!
//! A source key and its event commit together. The run stream is an outbox:
//! retrying a delivery after a crash returns the original event without
//! allocating another global sequence or publishing another live message.

use std::sync::Arc;

use chrono::Utc;
use fabro_types::{MainEvent, MainEventBody, MainEventEnvelope, RunId};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

use crate::{Error, Result};

const LIVE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainEventSourceKey {
    RunStream { run_id: RunId, item_id: String },
}

impl MainEventSourceKey {
    fn store_key(&self) -> String {
        let Self::RunStream { run_id, item_id } = self;
        format!("runs:{run_id}:{item_id}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppendMainEventOutcome {
    Appended(MainEventEnvelope),
    Duplicate(MainEventEnvelope),
}

#[derive(Clone)]
pub struct MainEventStore {
    pool:        SqlitePool,
    append_lock: Arc<Mutex<()>>,
    live:        broadcast::Sender<MainEventEnvelope>,
}

impl std::fmt::Debug for MainEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainEventStore").finish_non_exhaustive()
    }
}

impl MainEventStore {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        let (live, _) = broadcast::channel(LIVE_CAPACITY);
        Self {
            pool,
            append_lock: Arc::new(Mutex::new(())),
            live,
        }
    }

    pub async fn append(&self, body: MainEventBody) -> Result<MainEventEnvelope> {
        match self.append_inner(None, body).await? {
            AppendMainEventOutcome::Appended(event) | AppendMainEventOutcome::Duplicate(event) => {
                Ok(event)
            }
        }
    }

    pub async fn append_once(
        &self,
        source: MainEventSourceKey,
        body: MainEventBody,
    ) -> Result<AppendMainEventOutcome> {
        self.append_inner(Some(source.store_key()), body).await
    }

    async fn append_inner(
        &self,
        source_key: Option<String>,
        body: MainEventBody,
    ) -> Result<AppendMainEventOutcome> {
        // Keep broadcasts in commit order for all clones of this handle.
        // SQLite's write lock also serializes independent store handles.
        let _guard = self.append_lock.lock().await;
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let existing = match &source_key {
            Some(source_key) => {
                sqlx::query_as::<_, (i64, String)>(
                    "SELECT seq, event_json FROM main_events WHERE source_key = ?",
                )
                .bind(source_key)
                .fetch_optional(&mut *transaction)
                .await?
            }
            None => None,
        };
        if let Some((seq, json)) = existing {
            let event = decode_event(seq, &json)?;
            transaction.commit().await?;
            return Ok(AppendMainEventOutcome::Duplicate(event));
        }
        let last_seq: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM main_events")
            .fetch_one(&mut *transaction)
            .await?;
        let seq = u32::try_from(last_seq.unwrap_or(0))
            .ok()
            .and_then(|last| last.checked_add(1))
            .ok_or(Error::EventSequenceExhausted { max_seq: u32::MAX })?;
        let event = MainEvent {
            id: Uuid::now_v7().to_string(),
            ts: Utc::now(),
            body,
        };
        sqlx::query("INSERT INTO main_events (seq, source_key, event_json) VALUES (?, ?, ?)")
            .bind(i64::from(seq))
            .bind(source_key)
            .bind(serde_json::to_string(&event)?)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        let envelope = MainEventEnvelope { seq, event };
        let _ = self.live.send(envelope.clone());
        Ok(AppendMainEventOutcome::Appended(envelope))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MainEventEnvelope> {
        self.live.subscribe()
    }

    /// Replay from an inclusive sequence, returning at most `limit + 1`
    /// events so a consumer can determine whether another page exists.
    pub async fn list_from_with_limit(
        &self,
        start_seq: u32,
        limit: usize,
    ) -> Result<Vec<MainEventEnvelope>> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT seq, event_json FROM main_events WHERE seq >= ? ORDER BY seq LIMIT ?",
        )
        .bind(i64::from(start_seq))
        .bind(i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(seq, json)| decode_event(seq, &json))
            .collect()
    }
}

fn decode_event(seq: i64, json: &str) -> Result<MainEventEnvelope> {
    Ok(MainEventEnvelope {
        seq:   u32::try_from(seq)
            .map_err(|_| Error::EventSequenceExhausted { max_seq: u32::MAX })?,
        event: serde_json::from_str(json)?,
    })
}

#[cfg(test)]
mod tests {
    use fabro_types::{MainEventLifecycleProps, MainEventSource, fixtures, test_support};
    use tokio::sync::broadcast::error::TryRecvError;

    use super::*;
    use crate::platform_records::{PlatformRecord, RunCreatedRecord};
    use crate::test_support as store_test_support;

    fn body(run_id: RunId) -> MainEventBody {
        MainEventBody::RunRunning(MainEventLifecycleProps {
            source: MainEventSource {
                run_id,
                source_event_id: "platform:1".into(),
                source_event_ts: Utc::now(),
                source_event_name: "run.running".into(),
                actor: None,
            },
        })
    }

    fn source(run_id: RunId) -> MainEventSourceKey {
        MainEventSourceKey::RunStream {
            run_id,
            item_id: "platform:1".into(),
        }
    }

    #[tokio::test]
    async fn main_events_replay_and_broadcast_in_global_order() {
        let database = store_test_support::test_database();
        let store = database.main_events();
        let mut live = store.subscribe();
        let first = store.append(body(fixtures::RUN_1)).await.unwrap();
        let second = store.append(body(fixtures::RUN_2)).await.unwrap();
        let third = store.append(body(fixtures::RUN_1)).await.unwrap();
        assert_eq!((first.seq, second.seq, third.seq), (1, 2, 3));
        assert_ne!(first.event.id, second.event.id);
        assert_eq!(live.recv().await.unwrap(), first);
        assert_eq!(live.recv().await.unwrap(), second);
        assert_eq!(live.recv().await.unwrap(), third);
        assert_eq!(store.list_from_with_limit(2, 1).await.unwrap(), vec![
            second, third
        ]);
        assert!(store.list_from_with_limit(4, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn main_event_source_survives_reopen_and_is_scoped_to_run() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fabro.sqlite3");
        let database = fabro_db::Database::connect(&path).await.unwrap();
        database.migrate().await.unwrap();
        let store = MainEventStore::new(database.clone_pool());
        let AppendMainEventOutcome::Appended(first) = store
            .append_once(source(fixtures::RUN_1), body(fixtures::RUN_1))
            .await
            .unwrap()
        else {
            panic!("first append");
        };
        database.pool().close().await;
        drop(store);
        drop(database);

        let database = fabro_db::Database::connect(&path).await.unwrap();
        database.migrate().await.unwrap();
        let reopened = MainEventStore::new(database.clone_pool());
        let mut live = reopened.subscribe();
        let duplicate = reopened
            .append_once(source(fixtures::RUN_1), body(fixtures::RUN_1))
            .await
            .unwrap();
        assert_eq!(duplicate, AppendMainEventOutcome::Duplicate(first.clone()));
        assert_eq!(live.try_recv(), Err(TryRecvError::Empty));
        let AppendMainEventOutcome::Appended(second) = reopened
            .append_once(source(fixtures::RUN_2), body(fixtures::RUN_2))
            .await
            .unwrap()
        else {
            panic!("a different run has a different source key");
        };
        assert_eq!(second.seq, 2);
        assert_eq!(reopened.list_from_with_limit(1, 10).await.unwrap(), vec![
            first, second
        ]);
    }

    #[tokio::test]
    async fn concurrent_main_event_sources_consume_one_sequence_across_handles() {
        let directory = tempfile::tempdir().unwrap();
        let database = fabro_db::Database::connect(directory.path().join("fabro.sqlite3"))
            .await
            .unwrap();
        database.migrate().await.unwrap();
        let left = MainEventStore::new(database.clone_pool());
        let right = MainEventStore::new(database.clone_pool());
        let (first, second) = tokio::join!(
            left.append_once(source(fixtures::RUN_1), body(fixtures::RUN_1)),
            right.append_once(source(fixtures::RUN_1), body(fixtures::RUN_1)),
        );
        assert_ne!(
            matches!(first.unwrap(), AppendMainEventOutcome::Appended(_)),
            matches!(second.unwrap(), AppendMainEventOutcome::Appended(_))
        );
        assert_eq!(left.list_from_with_limit(1, 10).await.unwrap().len(), 1);
        assert_eq!(right.append(body(fixtures::RUN_2)).await.unwrap().seq, 2);
    }

    #[tokio::test]
    async fn failed_main_event_append_leaves_no_receipt_or_broadcast() {
        let database = store_test_support::test_database();
        let store = database.main_events();
        let pool = database.run_summary_store().pool();
        // Connect the lazy fixture and then inject a database write failure.
        store.list_from_with_limit(1, 1).await.unwrap();
        sqlx::raw_sql("CREATE TRIGGER fail_main_event BEFORE INSERT ON main_events BEGIN SELECT RAISE(ABORT, 'injected failure'); END;")
            .execute(&pool).await.unwrap();
        let mut live = store.subscribe();
        assert!(
            store
                .append_once(source(fixtures::RUN_1), body(fixtures::RUN_1))
                .await
                .is_err()
        );
        assert_eq!(live.try_recv(), Err(TryRecvError::Empty));
        sqlx::query("DROP TRIGGER fail_main_event")
            .execute(&pool)
            .await
            .unwrap();
        let AppendMainEventOutcome::Appended(event) = store
            .append_once(source(fixtures::RUN_1), body(fixtures::RUN_1))
            .await
            .unwrap()
        else {
            panic!("retry must append");
        };
        assert_eq!(event.seq, 1);
        assert_eq!(live.recv().await.unwrap(), event);
    }

    #[tokio::test]
    async fn main_event_sequence_exhaustion_preserves_existing_events() {
        let database = store_test_support::test_database();
        let store = database.main_events();
        let first = store.append(body(fixtures::RUN_1)).await.unwrap();
        sqlx::query("UPDATE main_events SET seq = ? WHERE seq = 1")
            .bind(i64::from(u32::MAX))
            .execute(&database.run_summary_store().pool())
            .await
            .unwrap();
        assert!(matches!(
            store.append(body(fixtures::RUN_2)).await,
            Err(Error::EventSequenceExhausted { .. })
        ));
        let events = store.list_from_with_limit(u32::MAX, 1).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, first.event);
    }

    #[tokio::test]
    async fn deleting_a_run_keeps_its_main_events_and_source_receipts() {
        let database = store_test_support::test_database();
        let spec = test_support::test_run_spec();
        let record = PlatformRecord::RunCreated(RunCreatedRecord {
            spec,
            title: None,
            parent_id: None,
            retried_from: None,
            web_url: None,
        });
        database
            .run_summary_store()
            .platform_records()
            .append(&fixtures::RUN_1, &record, None)
            .await
            .unwrap();
        let events = database.main_events();
        let first = events
            .append_once(source(fixtures::RUN_1), body(fixtures::RUN_1))
            .await
            .unwrap();
        database.delete_run(&fixtures::RUN_1).await.unwrap();
        let duplicate = events
            .append_once(source(fixtures::RUN_1), body(fixtures::RUN_1))
            .await
            .unwrap();
        let AppendMainEventOutcome::Appended(first) = first else {
            panic!("first append");
        };
        assert_eq!(duplicate, AppendMainEventOutcome::Duplicate(first.clone()));
        assert_eq!(events.list_from_with_limit(1, 10).await.unwrap(), vec![
            first
        ]);
    }
}
