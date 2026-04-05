use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use futures::Stream;
use serde::de::DeserializeOwned;
use slatedb::{CloseReason, Db, DbRead, ErrorKind};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::keys;
use crate::run_state::EventProjectionCache;
use crate::{EventEnvelope, EventPayload, Result, RunProjection, RunSummary, StageId, StoreError};
use fabro_types::{RunBlobId, RunId};

const DEFAULT_EVENT_TAIL_LIMIT: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeArtifact {
    pub node: StageId,
    pub filename: String,
}

#[derive(Clone)]
pub struct SlateRunStore {
    inner: Arc<SlateRunStoreInner>,
    read_only: bool,
}

impl std::fmt::Debug for SlateRunStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlateRunStore")
            .field("run_id", &self.inner.run_id)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

pub(crate) struct SlateRunStoreInner {
    run_id: RunId,
    db: Db,
    event_seq: AtomicU32,
    close_lock: Mutex<()>,
    projection_cache: Mutex<EventProjectionCache>,
    recent_events: Mutex<VecDeque<EventEnvelope>>,
    recent_event_limit: usize,
    event_tx: broadcast::Sender<EventEnvelope>,
}

impl SlateRunStore {
    pub(crate) async fn open_writer(run_id: RunId, db: Db) -> Result<Self> {
        let event_seq = recover_next_seq(&db, &keys::events_prefix(&run_id), keys::parse_event_seq).await?;
        let (event_tx, _) = broadcast::channel(DEFAULT_EVENT_TAIL_LIMIT.max(16));
        Ok(Self {
            inner: Arc::new(SlateRunStoreInner {
                run_id,
                db,
                event_seq: AtomicU32::new(event_seq),
                close_lock: Mutex::new(()),
                projection_cache: Mutex::new(EventProjectionCache::default()),
                recent_events: Mutex::new(VecDeque::with_capacity(DEFAULT_EVENT_TAIL_LIMIT)),
                recent_event_limit: DEFAULT_EVENT_TAIL_LIMIT,
                event_tx,
            }),
            read_only: false,
        })
    }

    pub(crate) async fn open_reader(run_id: RunId, db: Db) -> Result<Self> {
        let event_seq = recover_next_seq(&db, &keys::events_prefix(&run_id), keys::parse_event_seq).await?;
        let (event_tx, _) = broadcast::channel(DEFAULT_EVENT_TAIL_LIMIT.max(16));
        Ok(Self {
            inner: Arc::new(SlateRunStoreInner {
                run_id,
                db,
                event_seq: AtomicU32::new(event_seq),
                close_lock: Mutex::new(()),
                projection_cache: Mutex::new(EventProjectionCache::default()),
                recent_events: Mutex::new(VecDeque::with_capacity(DEFAULT_EVENT_TAIL_LIMIT)),
                recent_event_limit: DEFAULT_EVENT_TAIL_LIMIT,
                event_tx,
            }),
            read_only: true,
        })
    }

    pub(crate) fn from_inner(inner: Arc<SlateRunStoreInner>) -> Self {
        Self {
            inner,
            read_only: false,
        }
    }

    pub(crate) fn into_read_only(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            read_only: true,
        }
    }

    pub(crate) fn inner_arc(&self) -> Arc<SlateRunStoreInner> {
        Arc::clone(&self.inner)
    }

    pub(crate) fn run_id(&self) -> RunId {
        self.inner.run_id
    }

    pub(crate) fn matches_run(&self, run_id: &RunId) -> bool {
        self.inner.run_id == *run_id
    }

    pub(crate) async fn close(&self) -> Result<()> {
        let _guard = self.inner.close_lock.lock().await;
        if Arc::strong_count(&self.inner) <= 1 {
            match self.inner.db.close().await {
                Ok(()) => Ok(()),
                Err(err) if matches!(err.kind(), ErrorKind::Closed(CloseReason::Clean)) => Ok(()),
                Err(err) => Err(err.into()),
            }
        } else {
            Ok(())
        }
    }

    pub(crate) async fn validate_init<R>(db: &R, run_id: &RunId) -> Result<bool>
    where
        R: DbRead + Sync,
    {
        match get_json::<R, RunId>(db, &keys::init_key(run_id)).await? {
            Some(existing) if existing == *run_id => Ok(true),
            Some(existing) => Err(StoreError::Other(format!(
                "existing init record {existing:?} does not match requested run_id {run_id:?}"
            ))),
            None => Ok(false),
        }
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

impl SlateRunStore {
    pub async fn append_event(&self, payload: &EventPayload) -> Result<u32> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        payload.validate(&self.inner.run_id)?;
        let seq = self.inner.event_seq.fetch_add(1, Ordering::SeqCst);
        let event = EventEnvelope {
            seq,
            payload: payload.clone(),
        };
        self.inner.db.put(
            keys::event_key(&self.inner.run_id, seq, Utc::now().timestamp_millis()),
            serde_json::to_vec(payload)?,
        ).await?;
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

            let mut rx = inner.event_tx.subscribe();
            while let Ok(event) = rx.recv().await {
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
        let id = RunBlobId::new(&self.inner.run_id, data);
        self.inner
            .db
            .put(keys::blob_key(&self.inner.run_id, &id), data)
            .await?;
        Ok(id)
    }

    pub async fn read_blob(&self, id: &RunBlobId) -> Result<Option<Bytes>> {
        Ok(self
            .inner
            .db
            .get(keys::blob_key(&self.inner.run_id, id))
            .await?)
    }

    pub async fn list_blobs(&self) -> Result<Vec<RunBlobId>> {
        list_blobs(&self.inner.db, &self.inner.run_id).await
    }

    pub async fn put_artifact(&self, node: &StageId, filename: &str, data: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        self.inner
            .db
            .put(keys::node_artifact(&self.inner.run_id, node, filename), data)
            .await?;
        Ok(())
    }

    pub async fn get_artifact(&self, node: &StageId, filename: &str) -> Result<Option<Bytes>> {
        Ok(self
            .inner
            .db
            .get(keys::node_artifact(&self.inner.run_id, node, filename))
            .await?)
    }

    pub async fn list_all_artifacts(&self) -> Result<Vec<NodeArtifact>> {
        list_all_artifacts(&self.inner.db, &self.inner.run_id).await
    }

    pub async fn list_artifacts_for_stage(&self, stage_id: &StageId) -> Result<Vec<String>> {
        list_artifacts_for_stage(&self.inner.db, &self.inner.run_id, stage_id).await
    }

    pub async fn state(&self) -> Result<RunProjection> {
        self.projected_state().await
    }
}

async fn get_json<R, T>(db: &R, key: &str) -> Result<Option<T>>
where
    R: DbRead + Sync,
    T: DeserializeOwned,
{
    db.get(key)
        .await?
        .map(|value| serde_json::from_slice(&value))
        .transpose()
        .map_err(Into::into)
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
    let mut iter = db.scan_prefix(keys::events_prefix(run_id).as_bytes()).await?;
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

async fn list_blobs<R>(db: &R, run_id: &RunId) -> Result<Vec<RunBlobId>>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(keys::blobs_prefix(run_id).as_bytes()).await?;
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

async fn list_all_artifacts<R>(db: &R, run_id: &RunId) -> Result<Vec<NodeArtifact>>
where
    R: DbRead + Sync,
{
    let mut iter = db
        .scan_prefix(keys::run_prefix(run_id).as_bytes())
        .await?;
    let mut assets = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some((node, filename)) = keys::parse_node_artifact_key(&key) else {
            continue;
        };
        assets.push(NodeArtifact { node, filename });
    }
    assets.sort();
    Ok(assets)
}

async fn list_artifacts_for_stage<R>(db: &R, run_id: &RunId, stage_id: &StageId) -> Result<Vec<String>>
where
    R: DbRead + Sync,
{
    let prefix = keys::node_artifact_prefix(run_id, stage_id);
    let mut iter = db.scan_prefix(prefix.as_bytes()).await?;
    let mut filenames = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some((node, filename)) = keys::parse_node_artifact_key(&key) else {
            continue;
        };
        if &node == stage_id {
            filenames.push(filename);
        }
    }
    filenames.sort();
    Ok(filenames)
}

fn key_to_string(key: &Bytes) -> Result<String> {
    String::from_utf8(key.to_vec())
        .map_err(|err| StoreError::Other(format!("stored key is not valid UTF-8: {err}")))
}
