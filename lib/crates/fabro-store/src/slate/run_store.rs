use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use chrono::Utc;
use fabro_types::{RunBlobId, RunEvent, RunId, RunSummary};
use futures::Stream;
use slatedb::{Db, DbRead};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::blob_store::BlobStore;
use crate::run_state::{EventProjectionCache, RunProjectionReducer, build_summary};
use crate::{Error, EventEnvelope, EventPayload, Result, RunProjection, keys};

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
    blob_store:         BlobStore,
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
        Self::build(run_id, db, false).await
    }

    pub(crate) async fn open_reader(run_id: RunId, db: Db) -> Result<Self> {
        Self::build(run_id, db, true).await
    }

    async fn build(run_id: RunId, db: Db, read_only: bool) -> Result<Self> {
        let event_seq =
            recover_next_seq(&db, keys::run_events_prefix(&run_id), keys::parse_event_seq).await?;
        let (event_tx, _) = broadcast::channel(DEFAULT_EVENT_TAIL_LIMIT.max(16));
        let blob_store = BlobStore::new(Arc::new(db.clone()));
        Ok(Self {
            inner: Arc::new(RunDatabaseInner {
                run_id,
                db,
                blob_store,
                event_seq: AtomicU32::new(event_seq),
                close_lock: Mutex::new(()),
                state_lock: Mutex::new(()),
                projection_cache: Mutex::new(EventProjectionCache::default()),
                recent_events: Mutex::new(VecDeque::with_capacity(DEFAULT_EVENT_TAIL_LIMIT)),
                recent_event_limit: DEFAULT_EVENT_TAIL_LIMIT,
                event_tx,
            }),
            read_only,
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
        let mut iter = db.scan_prefix(keys::run_events_prefix(run_id)).await?;
        Ok(iter.next().await?.is_some())
    }

    pub(crate) async fn build_summary<R>(db: &R, run_id: &RunId) -> Result<RunSummary>
    where
        R: DbRead + Sync,
    {
        let events = list_events_from(db, run_id, 1).await?;
        let state = RunProjection::apply_events(&events)?;
        Ok(build_summary(&state, run_id))
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
            return Err(Error::ReadOnly);
        }
        payload.validate(&self.inner.run_id)?;
        let _state_guard = self.inner.state_lock.lock().await;
        let seq = self.inner.event_seq.fetch_add(1, Ordering::SeqCst);
        let event = EventEnvelope {
            seq,
            event: RunEvent::try_from(payload)?,
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

    /// Returns up to `limit + 1` events for the given workflow node,
    /// starting at `start_seq`. The `+1` lets callers compute `has_more`.
    ///
    /// Implementation note: scans the unbounded run-event prefix and
    /// filters by `node_id` *before* applying `limit`, so a stage with
    /// matches sparsely scattered late in the event log still returns its
    /// full slice (no premature truncation from a generic `limit`-bounded
    /// scan).
    pub async fn list_events_for_node_from_with_limit(
        &self,
        node_id: &str,
        start_seq: u32,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>> {
        list_events_for_node_from_with_limit(
            &self.inner.db,
            &self.inner.run_id,
            node_id,
            start_seq,
            limit,
        )
        .await
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
            return Err(Error::ReadOnly);
        }
        self.inner.blob_store.write(data).await
    }

    pub async fn read_blob(&self, id: &RunBlobId) -> Result<Option<Bytes>> {
        self.inner.blob_store.read(id).await
    }

    pub async fn list_blobs(&self) -> Result<Vec<RunBlobId>> {
        list_blobs(&self.inner.db).await
    }

    pub async fn state(&self) -> Result<RunProjection> {
        self.projected_state().await
    }
}

async fn recover_next_seq<R>(
    db: &R,
    prefix: keys::SlateKey,
    parse: fn(&str) -> Option<u32>,
) -> Result<u32>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(prefix).await?;
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
    let mut iter = db.scan_prefix(keys::run_events_prefix(run_id)).await?;
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
            event: serde_json::from_slice(&entry.value)?,
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

async fn list_events_for_node_from_with_limit<R>(
    db: &R,
    run_id: &RunId,
    node_id: &str,
    start_seq: u32,
    limit: usize,
) -> Result<Vec<EventEnvelope>>
where
    R: DbRead + Sync,
{
    // Unbounded scan first: filtering by node_id with a generic
    // limit-bounded scan would silently drop matches whenever the stage's
    // events are sparse late in the event log.
    let mut iter = db.scan_prefix(keys::run_events_prefix(run_id)).await?;
    let mut events: Vec<EventEnvelope> = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some(seq) = keys::parse_event_seq(&key) else {
            continue;
        };
        if seq < start_seq {
            continue;
        }
        let event: RunEvent = serde_json::from_slice(&entry.value)?;
        if event.node_id.as_deref() != Some(node_id) {
            continue;
        }
        events.push(EventEnvelope { seq, event });
    }
    events.sort_by_key(|event| event.seq);
    events.truncate(limit.saturating_add(1));
    Ok(events)
}

async fn list_blobs<R>(db: &R) -> Result<Vec<RunBlobId>>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(keys::blobs_prefix()).await?;
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
        .map_err(|err| Error::Other(format!("stored key is not valid UTF-8: {err}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_types::RunId;
    use object_store::memory::InMemory;
    use serde_json::json;

    use crate::{Database, EventPayload};

    #[tokio::test]
    async fn list_blobs_reads_global_cas_namespace() {
        let object_store = Arc::new(InMemory::new());
        let store = Database::new(object_store, "", Duration::from_millis(1), None);
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

    fn stage_prompt_payload(run_id: &RunId, idx: u32, node_id: Option<&str>) -> EventPayload {
        let mut value = json!({
            "id": format!("evt-{idx}"),
            "ts": "2026-04-09T12:00:00Z",
            "run_id": run_id.to_string(),
            "event": "stage.prompt",
            "properties": {
                "visit": 1,
                "text": format!("prompt {idx}"),
            },
        });
        if let Some(node_id) = node_id {
            value
                .as_object_mut()
                .unwrap()
                .insert("node_id".into(), json!(node_id));
        }
        EventPayload::new(value, run_id).unwrap()
    }

    async fn fresh_run() -> super::RunDatabase {
        let object_store = Arc::new(InMemory::new());
        let store = Database::new(object_store, "", Duration::from_millis(1), None);
        let run_id: RunId = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        store.create_run(&run_id).await.unwrap()
    }

    #[tokio::test]
    async fn list_events_for_node_returns_only_matching_events_in_seq_order() {
        let run = fresh_run().await;
        let run_id = run.run_id();
        run.append_event(&stage_prompt_payload(&run_id, 1, Some("alpha")))
            .await
            .unwrap();
        run.append_event(&stage_prompt_payload(&run_id, 2, Some("beta")))
            .await
            .unwrap();
        run.append_event(&stage_prompt_payload(&run_id, 3, Some("alpha")))
            .await
            .unwrap();

        let events = run
            .list_events_for_node_from_with_limit("alpha", 1, 100)
            .await
            .unwrap();

        let seqs: Vec<u32> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 3]);
    }

    #[tokio::test]
    async fn list_events_for_node_skips_events_with_no_node_id() {
        let run = fresh_run().await;
        let run_id = run.run_id();
        run.append_event(&stage_prompt_payload(&run_id, 1, None))
            .await
            .unwrap();
        run.append_event(&stage_prompt_payload(&run_id, 2, Some("alpha")))
            .await
            .unwrap();

        let events = run
            .list_events_for_node_from_with_limit("alpha", 1, 100)
            .await
            .unwrap();

        let seqs: Vec<u32> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![2]);
    }

    #[tokio::test]
    async fn list_events_for_node_paginates_via_start_seq_on_filtered_slice() {
        let run = fresh_run().await;
        let run_id = run.run_id();
        for idx in 1..=5 {
            let node = if idx % 2 == 0 { "beta" } else { "alpha" };
            run.append_event(&stage_prompt_payload(&run_id, idx, Some(node)))
                .await
                .unwrap();
        }

        // alpha events live at seqs 1, 3, 5. Start at seq=2 should skip seq=1.
        let events = run
            .list_events_for_node_from_with_limit("alpha", 2, 100)
            .await
            .unwrap();

        let seqs: Vec<u32> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![3, 5]);
    }

    #[tokio::test]
    async fn list_events_for_node_walks_past_unrelated_events_for_sparse_matches() {
        let run = fresh_run().await;
        let run_id = run.run_id();
        // 200 unrelated events first.
        for idx in 1..=200 {
            run.append_event(&stage_prompt_payload(&run_id, idx, Some("noise")))
                .await
                .unwrap();
        }
        // Then 3 sparse "alpha" events at the tail.
        for idx in 201..=203 {
            run.append_event(&stage_prompt_payload(&run_id, idx, Some("alpha")))
                .await
                .unwrap();
        }

        // limit smaller than the number of unrelated events would have
        // truncated the upstream scan if we had post-filtered.
        let events = run
            .list_events_for_node_from_with_limit("alpha", 1, 5)
            .await
            .unwrap();

        let seqs: Vec<u32> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![201, 202, 203]);
    }

    #[tokio::test]
    async fn list_events_for_node_returns_limit_plus_one_for_has_more_signal() {
        let run = fresh_run().await;
        let run_id = run.run_id();
        for idx in 1..=5 {
            run.append_event(&stage_prompt_payload(&run_id, idx, Some("alpha")))
                .await
                .unwrap();
        }

        let events = run
            .list_events_for_node_from_with_limit("alpha", 1, 2)
            .await
            .unwrap();

        // With limit=2, we expect up to limit+1 = 3 envelopes so the
        // caller can compute has_more.
        assert_eq!(events.len(), 3);
    }
}
