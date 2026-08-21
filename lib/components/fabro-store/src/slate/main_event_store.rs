use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use chrono::Utc;
use fabro_types::{MainEvent, MainEventBody, MainEventEnvelope};
use slatedb::{Db, DbRead};
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

use crate::{Error, Result, keys};

const DEFAULT_MAIN_EVENT_TAIL_LIMIT: usize = 1024;

#[derive(Clone)]
pub struct MainEventStore {
    inner: Arc<MainEventStoreInner>,
}

impl std::fmt::Debug for MainEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainEventStore").finish_non_exhaustive()
    }
}

struct MainEventStoreInner {
    db:          Db,
    event_seq:   AtomicU32,
    append_lock: Mutex<()>,
    event_tx:    broadcast::Sender<MainEventEnvelope>,
}

impl MainEventStore {
    pub(crate) async fn open(db: Db) -> Result<Self> {
        let event_seq = recover_next_seq(&db).await?;
        let (event_tx, _) = broadcast::channel(DEFAULT_MAIN_EVENT_TAIL_LIMIT.max(16));
        Ok(Self {
            inner: Arc::new(MainEventStoreInner {
                db,
                event_seq: AtomicU32::new(event_seq),
                append_lock: Mutex::new(()),
                event_tx,
            }),
        })
    }

    pub async fn append(&self, body: MainEventBody) -> Result<MainEventEnvelope> {
        let _guard = self.inner.append_lock.lock().await;
        let seq = self.inner.event_seq.fetch_add(1, Ordering::SeqCst);
        let now = Utc::now();
        let event = MainEvent {
            id: Uuid::now_v7().to_string(),
            ts: now,
            body,
        };
        self.inner
            .db
            .put(
                keys::main_event_key(seq, now.timestamp_millis()),
                serde_json::to_vec(&event)?,
            )
            .await?;
        let envelope = MainEventEnvelope { seq, event };
        let _ = self.inner.event_tx.send(envelope.clone());
        Ok(envelope)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MainEventEnvelope> {
        self.inner.event_tx.subscribe()
    }

    pub async fn list_from_with_limit(
        &self,
        start_seq: u32,
        limit: usize,
    ) -> Result<Vec<MainEventEnvelope>> {
        let mut events = list_events_from(&self.inner.db, start_seq).await?;
        events.truncate(limit.saturating_add(1));
        Ok(events)
    }
}

async fn recover_next_seq<R>(db: &R) -> Result<u32>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(keys::main_events_prefix()).await?;
    let mut max_seq = 0;
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        if let Some(seq) = keys::parse_main_event_seq(&key) {
            max_seq = max_seq.max(seq);
        }
    }
    Ok(max_seq.saturating_add(1).max(1))
}

async fn list_events_from<R>(db: &R, start_seq: u32) -> Result<Vec<MainEventEnvelope>>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(keys::main_events_prefix()).await?;
    let mut events = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some(seq) = keys::parse_main_event_seq(&key) else {
            continue;
        };
        if seq < start_seq {
            continue;
        }
        events.push(MainEventEnvelope {
            seq,
            event: serde_json::from_slice(&entry.value)?,
        });
    }
    events.sort_by_key(|event| event.seq);
    Ok(events)
}

fn key_to_string(key: &Bytes) -> Result<String> {
    String::from_utf8(key.to_vec())
        .map_err(|err| Error::Other(format!("stored key is not valid UTF-8: {err}")))
}
