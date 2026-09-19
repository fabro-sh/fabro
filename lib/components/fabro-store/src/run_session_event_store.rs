//! SQLite storage for the events of Ask Fabro sessions.
//!
//! A session's events are numbered per session, from 1, in the order they
//! are appended; an append allocates the next number under the write lock,
//! so two writers of one session never share a number. Every committed
//! event is also published to the store's subscribers, which is how a
//! client attached to a session sees a turn as it runs.

use chrono::{DateTime, Utc};
use fabro_types::{RunId, SessionEvent, SessionEventBody, SessionId};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, SqlitePool};
use tokio::sync::broadcast;

use crate::{Error, Result, sqlite_row};

const RECORD_NAME: &str = "run session event";

/// How many committed events a slow subscriber may fall behind before it
/// is told it lagged and has to replay from the table.
const LIVE_CAPACITY: usize = 1024;

/// Reads and writes session events in SQLite and publishes each committed
/// one.
pub struct RunSessionEventStore {
    pool: SqlitePool,
    live: broadcast::Sender<SessionEvent>,
}

impl std::fmt::Debug for RunSessionEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunSessionEventStore")
            .finish_non_exhaustive()
    }
}

impl RunSessionEventStore {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        let (live, _) = broadcast::channel(LIVE_CAPACITY);
        Self { pool, live }
    }

    /// Record `body` as the session's next event and publish it.
    pub async fn append(
        &self,
        run_id: RunId,
        session_id: SessionId,
        body: SessionEventBody,
        ts: DateTime<Utc>,
    ) -> Result<SessionEvent> {
        let properties = serde_json::to_value(&body)?;
        let properties_json =
            serde_json::to_string(properties.get("properties").ok_or_else(|| {
                Error::InvalidEvent("session event body has no properties".into())
            })?)?;
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let last_seq: Option<i64> =
            sqlx::query_scalar("SELECT MAX(seq) FROM run_session_events WHERE session_id = ?")
                .bind(session_id.to_string())
                .fetch_one(&mut *transaction)
                .await?;
        let seq = u32::try_from(last_seq.unwrap_or(0))
            .ok()
            .and_then(|last| last.checked_add(1))
            .ok_or(Error::EventSequenceExhausted { max_seq: u32::MAX })?;
        sqlx::query(
            r"
INSERT INTO run_session_events (
    session_id, seq, run_id, turn_id, event_name, recorded_at_ms, properties_json
) VALUES (?, ?, ?, ?, ?, ?, ?)
",
        )
        .bind(session_id.to_string())
        .bind(i64::from(seq))
        .bind(run_id.to_string())
        .bind(body.turn_id().map(|turn_id| turn_id.to_string()))
        .bind(body.event_name())
        .bind(ts.timestamp_millis())
        .bind(properties_json)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        let event = SessionEvent {
            seq,
            session_id,
            run_id,
            ts,
            body,
        };
        // No subscriber is not an error: the event is committed either way.
        let _ = self.live.send(event.clone());
        Ok(event)
    }

    /// The session's events from `since_seq` on, at most `limit` of them,
    /// in sequence order.
    pub async fn list_from(
        &self,
        session_id: SessionId,
        since_seq: u32,
        limit: usize,
    ) -> Result<Vec<SessionEvent>> {
        let rows = sqlx::query(
            r"
SELECT session_id, seq, run_id, event_name, recorded_at_ms, properties_json
FROM run_session_events
WHERE session_id = ? AND seq >= ?
ORDER BY seq ASC
LIMIT ?
",
        )
        .bind(session_id.to_string())
        .bind(i64::from(since_seq))
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    /// Every event of every session of `run_id`, session by session in
    /// sequence order.
    pub async fn list_for_run(&self, run_id: RunId) -> Result<Vec<SessionEvent>> {
        let rows = sqlx::query(
            r"
SELECT session_id, seq, run_id, event_name, recorded_at_ms, properties_json
FROM run_session_events
WHERE run_id = ?
ORDER BY session_id ASC, seq ASC
",
        )
        .bind(run_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    /// The sequence number of the session's latest event, if it has any.
    pub async fn last_seq(&self, session_id: SessionId) -> Result<Option<u32>> {
        let last_seq: Option<i64> =
            sqlx::query_scalar("SELECT MAX(seq) FROM run_session_events WHERE session_id = ?")
                .bind(session_id.to_string())
                .fetch_one(&self.pool)
                .await?;
        last_seq
            .map(|seq| {
                u32::try_from(seq).map_err(|_| {
                    Error::InvalidEvent(format!("stored {RECORD_NAME} sequence {seq}"))
                })
            })
            .transpose()
    }

    /// The run that owns `session_id`, from the session's creation event.
    pub async fn owner(&self, session_id: SessionId) -> Result<Option<RunId>> {
        let run_id: Option<String> = sqlx::query_scalar(
            "SELECT run_id FROM run_session_events WHERE session_id = ? AND seq = 1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        run_id
            .map(|run_id| {
                run_id.parse::<RunId>().map_err(|error| {
                    Error::InvalidEvent(format!("stored {RECORD_NAME} run id: {error}"))
                })
            })
            .transpose()
    }

    /// Forget every session event of `run_id`.
    pub async fn delete_for_run(&self, run_id: RunId) -> Result<u64> {
        let result = sqlx::query("DELETE FROM run_session_events WHERE run_id = ?")
            .bind(run_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Every event committed from now on, across all sessions. A receiver
    /// that falls more than the channel's capacity behind gets
    /// `RecvError::Lagged` and replays from `list_from`.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.live.subscribe()
    }
}

fn event_from_row(row: &SqliteRow) -> Result<SessionEvent> {
    let session_id: String = row.try_get("session_id")?;
    let session_id = session_id.parse::<SessionId>().map_err(|error| {
        Error::InvalidEvent(format!("stored {RECORD_NAME} session id: {error}"))
    })?;
    let seq: i64 = row.try_get("seq")?;
    let seq = u32::try_from(seq)
        .map_err(|_| Error::InvalidEvent(format!("stored {RECORD_NAME} sequence {seq}")))?;
    let run_id: String = row.try_get("run_id")?;
    let run_id = run_id
        .parse::<RunId>()
        .map_err(|error| Error::InvalidEvent(format!("stored {RECORD_NAME} run id: {error}")))?;
    let ts = sqlite_row::timestamp_from_row(row, RECORD_NAME, "recorded_at_ms")?;
    let event_name: String = row.try_get("event_name")?;
    let properties_json: String = row.try_get("properties_json")?;
    let properties: serde_json::Value = serde_json::from_str(&properties_json)?;
    let body: SessionEventBody = serde_json::from_value(serde_json::json!({
        "event": event_name,
        "properties": properties,
    }))?;
    Ok(SessionEvent {
        seq,
        session_id,
        run_id,
        ts,
        body,
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use fabro_types::session_event::{
        SessionAssistantDeltaProps, SessionCreatedProps, SessionTurnStartedProps,
    };
    use fabro_types::{TurnId, fixtures};

    use super::*;
    use crate::test_support;

    fn store() -> RunSessionEventStore {
        RunSessionEventStore::new(test_support::in_memory_pool_with(&[
            fabro_db::RUN_SESSION_EVENTS_MIGRATION_SQL,
        ]))
    }

    fn created() -> SessionEventBody {
        SessionEventBody::Created(SessionCreatedProps {
            title:    Some("Ask Fabro".to_string()),
            model:    Some("test-model".to_string()),
            provider: None,
        })
    }

    fn turn_started(turn_id: TurnId) -> SessionEventBody {
        SessionEventBody::TurnStarted(SessionTurnStartedProps {
            turn_id,
            input: "What happened?".to_string(),
        })
    }

    #[tokio::test]
    async fn appends_number_a_session_from_one_and_read_back_in_order() {
        let store = store();
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let ts = Utc.with_ymd_and_hms(2026, 9, 18, 12, 0, 0).unwrap();

        let first = store
            .append(fixtures::RUN_1, session_id, created(), ts)
            .await
            .unwrap();
        let second = store
            .append(fixtures::RUN_1, session_id, turn_started(turn_id), ts)
            .await
            .unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
        assert_eq!(second.body.turn_id(), Some(turn_id));

        let events = store.list_from(session_id, 1, 10).await.unwrap();
        assert_eq!(events, vec![first.clone(), second.clone()]);
        assert_eq!(store.list_from(session_id, 2, 10).await.unwrap(), vec![
            second
        ]);
        assert_eq!(store.list_from(session_id, 1, 1).await.unwrap(), vec![
            first
        ]);
        assert_eq!(store.last_seq(session_id).await.unwrap(), Some(2));
        assert_eq!(store.last_seq(SessionId::new()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn sessions_are_numbered_apart_and_owned_by_their_run() {
        let store = store();
        let first = SessionId::new();
        let second = SessionId::new();
        let ts = Utc::now();
        store
            .append(fixtures::RUN_1, first, created(), ts)
            .await
            .unwrap();
        store
            .append(fixtures::RUN_1, first, turn_started(TurnId::new()), ts)
            .await
            .unwrap();
        let other = store
            .append(fixtures::RUN_2, second, created(), ts)
            .await
            .unwrap();
        assert_eq!(other.seq, 1);

        assert_eq!(store.owner(first).await.unwrap(), Some(fixtures::RUN_1));
        assert_eq!(store.owner(second).await.unwrap(), Some(fixtures::RUN_2));
        assert_eq!(store.owner(SessionId::new()).await.unwrap(), None);

        let run_events = store.list_for_run(fixtures::RUN_1).await.unwrap();
        assert_eq!(run_events.len(), 2);
        assert!(
            run_events
                .iter()
                .all(|event| event.session_id == first && event.run_id == fixtures::RUN_1)
        );

        assert_eq!(store.delete_for_run(fixtures::RUN_1).await.unwrap(), 2);
        assert!(
            store
                .list_for_run(fixtures::RUN_1)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.owner(first).await.unwrap(), None);
        assert_eq!(store.owner(second).await.unwrap(), Some(fixtures::RUN_2));
    }

    #[tokio::test]
    async fn a_subscriber_sees_each_committed_event() {
        let store = store();
        let session_id = SessionId::new();
        let mut live = store.subscribe();
        let ts = Utc::now();
        let appended = store
            .append(fixtures::RUN_1, session_id, created(), ts)
            .await
            .unwrap();
        let delta = store
            .append(
                fixtures::RUN_1,
                session_id,
                SessionEventBody::AssistantDelta(SessionAssistantDeltaProps {
                    turn_id: TurnId::new(),
                    delta:   "Hel".to_string(),
                }),
                ts,
            )
            .await
            .unwrap();

        assert_eq!(live.recv().await.unwrap(), appended);
        assert_eq!(live.recv().await.unwrap(), delta);
    }
}
