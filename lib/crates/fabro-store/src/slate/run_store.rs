use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use chrono::Utc;
use fabro_types::{RunBlobId, RunId};
use futures::Stream;
use slatedb::{Db, DbRead};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::run_state::EventProjectionCache;
use crate::{EventEnvelope, EventPayload, Result, RunProjection, RunSummary, StoreError, keys};

const DEFAULT_EVENT_TAIL_LIMIT: usize = 1024;
#[derive(Clone)]
pub struct RunDatabase {
    inner:     Arc<RunDatabaseInner>,
    read_only: bool,
}

impl std::fmt::Debug for RunDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunDatabase")
            .field("run_id", &self.inner.run_id)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

pub(crate) struct RunDatabaseInner {
    run_id:             RunId,
    db:                 Db,
    event_seq:          AtomicU32,
    close_lock:         Mutex<()>,
    state_lock:         Mutex<()>,
    projection_cache:   Mutex<EventProjectionCache>,
    recent_events:      Mutex<VecDeque<EventEnvelope>>,
    recent_event_limit: usize,
    event_tx:           broadcast::Sender<EventEnvelope>,
}

impl RunDatabase {
    pub(crate) async fn open_writer(run_id: RunId, db: Db) -> Result<Self> {
        let event_seq = recover_next_seq(
            &db,
            &keys::run_events_prefix(&run_id),
            keys::parse_event_seq,
        )
        .await?;
        let (event_tx, _) = broadcast::channel(DEFAULT_EVENT_TAIL_LIMIT.max(16));
        Ok(Self {
            inner:     Arc::new(RunDatabaseInner {
                run_id,
                db,
                event_seq: AtomicU32::new(event_seq),
                close_lock: Mutex::new(()),
                state_lock: Mutex::new(()),
                projection_cache: Mutex::new(EventProjectionCache::default()),
                recent_events: Mutex::new(VecDeque::with_capacity(DEFAULT_EVENT_TAIL_LIMIT)),
                recent_event_limit: DEFAULT_EVENT_TAIL_LIMIT,
                event_tx,
            }),
            read_only: false,
        })
    }

    pub(crate) async fn open_reader(run_id: RunId, db: Db) -> Result<Self> {
        let event_seq = recover_next_seq(
            &db,
            &keys::run_events_prefix(&run_id),
            keys::parse_event_seq,
        )
        .await?;
        let (event_tx, _) = broadcast::channel(DEFAULT_EVENT_TAIL_LIMIT.max(16));
        Ok(Self {
            inner:     Arc::new(RunDatabaseInner {
                run_id,
                db,
                event_seq: AtomicU32::new(event_seq),
                close_lock: Mutex::new(()),
                state_lock: Mutex::new(()),
                projection_cache: Mutex::new(EventProjectionCache::default()),
                recent_events: Mutex::new(VecDeque::with_capacity(DEFAULT_EVENT_TAIL_LIMIT)),
                recent_event_limit: DEFAULT_EVENT_TAIL_LIMIT,
                event_tx,
            }),
            read_only: true,
        })
    }

    pub(crate) fn from_inner(inner: Arc<RunDatabaseInner>) -> Self {
        Self {
            inner,
            read_only: false,
        }
    }

    pub(crate) fn read_only_clone(&self) -> Self {
        Self {
            inner:     Arc::clone(&self.inner),
            read_only: true,
        }
    }

    pub(crate) fn inner_arc(&self) -> Arc<RunDatabaseInner> {
        Arc::clone(&self.inner)
    }

    pub(crate) fn run_id(&self) -> RunId {
        self.inner.run_id
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.inner.event_tx.subscribe()
    }

    pub(crate) fn matches_run(&self, run_id: &RunId) -> bool {
        self.inner.run_id == *run_id
    }

    pub(crate) async fn close(&self) -> Result<()> {
        let _guard = self.inner.close_lock.lock().await;
        Ok(())
    }

    pub(crate) async fn has_any_events<R>(db: &R, run_id: &RunId) -> Result<bool>
    where
        R: DbRead + Sync,
    {
        let mut iter = db
            .scan_prefix(keys::run_events_prefix(run_id).as_bytes())
            .await?;
        Ok(iter.next().await?.is_some())
    }

    pub(crate) async fn build_summary<R>(db: &R, run_id: &RunId) -> Result<RunSummary>
    where
        R: DbRead + Sync,
    {
        let events = list_events_from(db, run_id, 1).await?;
        let state = RunProjection::apply_events(&events)?;
        Ok(state.build_summary(run_id))
    }

    async fn projected_state(&self) -> Result<RunProjection> {
        let _state_guard = self.inner.state_lock.lock().await;
        let next_seq = {
            let cache = self.inner.projection_cache.lock().await;
            cache.last_seq.saturating_add(1)
        };
        let events = list_events_from(&self.inner.db, &self.inner.run_id, next_seq).await?;
        let mut cache = self.inner.projection_cache.lock().await;
        for event in &events {
            cache.state.apply_event(event)?;
            cache.last_seq = event.seq;
        }
        Ok(cache.state.clone())
    }

    async fn cache_event(&self, event: &EventEnvelope) -> Result<()> {
        {
            let mut projection_cache = self.inner.projection_cache.lock().await;
            projection_cache.state.apply_event(event)?;
            projection_cache.last_seq = event.seq;
        }
        let mut recent_events = self.inner.recent_events.lock().await;
        recent_events.push_back(event.clone());
        while recent_events.len() > self.inner.recent_event_limit {
            recent_events.pop_front();
        }
        let _ = self.inner.event_tx.send(event.clone());
        Ok(())
    }

    async fn cached_events_from(&self, start_seq: u32, limit: usize) -> Option<Vec<EventEnvelope>> {
        let recent_events = self.inner.recent_events.lock().await;
        let oldest_seq = recent_events.front().map(|event| event.seq)?;
        if start_seq < oldest_seq {
            return None;
        }
        let mut events = recent_events
            .iter()
            .filter(|event| event.seq >= start_seq)
            .take(limit.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();
        if events.is_empty() && start_seq <= self.inner.event_seq.load(Ordering::SeqCst) {
            events = Vec::new();
        }
        Some(events)
    }
}

impl RunDatabase {
    pub async fn append_event(&self, payload: &EventPayload) -> Result<u32> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        payload.validate(&self.inner.run_id)?;
        let _state_guard = self.inner.state_lock.lock().await;
        let seq = self.inner.event_seq.fetch_add(1, Ordering::SeqCst);
        let event = EventEnvelope {
            seq,
            payload: payload.clone(),
        };
        self.inner
            .db
            .put(
                keys::run_event_key(&self.inner.run_id, seq, Utc::now().timestamp_millis()),
                serde_json::to_vec(payload)?,
            )
            .await?;
        self.cache_event(&event).await?;
        Ok(seq)
    }

    pub async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        self.list_events_from_with_limit(1, usize::MAX / 2).await
    }

    pub async fn list_events_from_with_limit(
        &self,
        start_seq: u32,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>> {
        if let Some(events) = self.cached_events_from(start_seq, limit).await {
            return Ok(events);
        }
        list_events_from_with_limit(&self.inner.db, &self.inner.run_id, start_seq, limit).await
    }

    pub fn watch_events_from(
        &self,
        seq: u32,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = Result<EventEnvelope>> + Send>>> {
        let inner = Arc::clone(&self.inner);
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut rx = inner.event_tx.subscribe();
            let cached = {
                let recent_events = inner.recent_events.lock().await;
                recent_events
                    .iter()
                    .filter(|event| event.seq >= seq)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let mut next_seq = seq;
            for event in cached {
                next_seq = event.seq.saturating_add(1);
                if sender.send(Ok(event)).is_err() {
                    return;
                }
            }

            loop {
                loop {
                    match rx.try_recv() {
                        Ok(event) => {
                            if event.seq < next_seq {
                                continue;
                            }
                            next_seq = event.seq.saturating_add(1);
                            if sender.send(Ok(event)).is_err() {
                                return;
                            }
                        }
                        Err(broadcast::error::TryRecvError::Empty) => break,
                        Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                        Err(broadcast::error::TryRecvError::Closed) => return,
                    }
                }

                let event = match rx.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if event.seq < next_seq {
                    continue;
                }
                next_seq = event.seq.saturating_add(1);
                if sender.send(Ok(event)).is_err() {
                    return;
                }
            }
        });
        Ok(Box::pin(UnboundedReceiverStream::new(receiver)))
    }

    pub async fn write_blob(&self, data: &[u8]) -> Result<RunBlobId> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        let id = RunBlobId::new(data);
        self.inner.db.put(keys::blob_key(&id), data).await?;
        Ok(id)
    }

    pub async fn read_blob(&self, id: &RunBlobId) -> Result<Option<Bytes>> {
        let global = self.inner.db.get(keys::blob_key(id)).await?;
        if global.is_some() {
            return Ok(global);
        }
        Ok(None)
    }

    pub async fn list_blobs(&self) -> Result<Vec<RunBlobId>> {
        list_blobs(&self.inner.db).await
    }

    pub async fn state(&self) -> Result<RunProjection> {
        self.projected_state().await
    }
}

async fn recover_next_seq<R>(db: &R, prefix: &str, parse: fn(&str) -> Option<u32>) -> Result<u32>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(prefix.as_bytes()).await?;
    let mut max_seq = 0;
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        if let Some(seq) = parse(&key) {
            max_seq = max_seq.max(seq);
        }
    }
    Ok(max_seq.saturating_add(1).max(1))
}

async fn list_events_from<R>(db: &R, run_id: &RunId, start_seq: u32) -> Result<Vec<EventEnvelope>>
where
    R: DbRead + Sync,
{
    let mut iter = db
        .scan_prefix(keys::run_events_prefix(run_id).as_bytes())
        .await?;
    let mut events = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some(seq) = keys::parse_event_seq(&key) else {
            continue;
        };
        if seq < start_seq {
            continue;
        }
        events.push(EventEnvelope {
            seq,
            payload: serde_json::from_slice(&entry.value)?,
        });
    }
    events.sort_by_key(|event| event.seq);
    Ok(events)
}

async fn list_events_from_with_limit<R>(
    db: &R,
    run_id: &RunId,
    start_seq: u32,
    limit: usize,
) -> Result<Vec<EventEnvelope>>
where
    R: DbRead + Sync,
{
    let mut events = list_events_from(db, run_id, start_seq).await?;
    events.truncate(limit.saturating_add(1));
    Ok(events)
}

async fn list_blobs<R>(db: &R) -> Result<Vec<RunBlobId>>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(keys::blobs_prefix().as_bytes()).await?;
    let mut blob_ids = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some(blob_id) = keys::parse_blob_id(&key) else {
            continue;
        };
        blob_ids.push(blob_id);
    }
    blob_ids.sort();
    Ok(blob_ids)
}

fn key_to_string(key: &Bytes) -> Result<String> {
    String::from_utf8(key.to_vec())
        .map_err(|err| StoreError::Other(format!("stored key is not valid UTF-8: {err}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use object_store::memory::InMemory;

    use crate::Database;
    #[tokio::test]
    async fn list_blobs_reads_global_cas_namespace() {
        let object_store = Arc::new(InMemory::new());
        let store = Database::new(object_store, "", Duration::from_millis(1));
        let run_id = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        let run = store.create_run(&run_id).await.unwrap();
        let first_blob = br#"{"a":1}"#;
        let second_blob = br#"{"b":2}"#;

        let first_id = run.write_blob(first_blob).await.unwrap();
        let second_id = run.write_blob(second_blob).await.unwrap();
        let mut blob_ids = run.list_blobs().await.unwrap();
        blob_ids.sort();

        assert_eq!(blob_ids, vec![first_id, second_id]);
    }
}
