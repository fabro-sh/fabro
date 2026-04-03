use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use slatedb::{CloseReason, DbRead, DbReader, ErrorKind};
use tokio::sync::{Mutex, mpsc};
use tokio::time;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::keys;
use crate::run_state::EventProjectionCache;
use crate::{
    CatalogRecord, EventEnvelope, EventPayload, NodeOutcomeRecord, NodeSnapshot, NodeVisitRef,
    Result, RunState, RunSummary, StoreError,
};
use fabro_types::{NodeStatusRecord, RunId};

#[derive(Clone)]
pub struct SlateRunStore {
    inner: Arc<SlateRunStoreInner>,
}

impl std::fmt::Debug for SlateRunStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlateRunStore")
            .field("run_id", &self.inner.run_id)
            .field("created_at", &self.inner.created_at)
            .field("db_prefix", &self.inner.db_prefix)
            .field("run_dir", &self.inner.run_dir)
            .finish_non_exhaustive()
    }
}

pub(crate) struct SlateRunStoreInner {
    run_id: RunId,
    created_at: DateTime<Utc>,
    db_prefix: String,
    run_dir: Option<String>,
    db: SlateRunDb,
    event_seq: AtomicU32,
    close_lock: Mutex<()>,
    projection_cache: Mutex<EventProjectionCache>,
}

enum SlateRunDb {
    Writer(slatedb::Db),
    Reader(Box<DbReader>),
}

impl SlateRunStore {
    pub(crate) async fn open_writer(record: CatalogRecord, db: slatedb::Db) -> Result<Self> {
        let event_seq = recover_next_seq(&db, keys::EVENTS_PREFIX, keys::parse_event_seq).await?;
        Ok(Self {
            inner: Arc::new(SlateRunStoreInner {
                run_id: record.run_id,
                created_at: record.created_at,
                db_prefix: record.db_prefix,
                run_dir: record.run_dir,
                db: SlateRunDb::Writer(db),
                event_seq: AtomicU32::new(event_seq),
                close_lock: Mutex::new(()),
                projection_cache: Mutex::new(EventProjectionCache::default()),
            }),
        })
    }

    pub(crate) async fn open_reader(record: CatalogRecord, db: DbReader) -> Result<Self> {
        let event_seq = recover_next_seq(&db, keys::EVENTS_PREFIX, keys::parse_event_seq).await?;
        Ok(Self {
            inner: Arc::new(SlateRunStoreInner {
                run_id: record.run_id,
                created_at: record.created_at,
                db_prefix: record.db_prefix,
                run_dir: record.run_dir,
                db: SlateRunDb::Reader(Box::new(db)),
                event_seq: AtomicU32::new(event_seq),
                close_lock: Mutex::new(()),
                projection_cache: Mutex::new(EventProjectionCache::default()),
            }),
        })
    }

    pub(crate) fn from_inner(inner: Arc<SlateRunStoreInner>) -> Self {
        Self { inner }
    }

    pub(crate) fn downgrade(&self) -> Weak<SlateRunStoreInner> {
        Arc::downgrade(&self.inner)
    }

    pub(crate) fn record(&self) -> CatalogRecord {
        CatalogRecord {
            run_id: self.inner.run_id,
            created_at: self.inner.created_at,
            db_prefix: self.inner.db_prefix.clone(),
            run_dir: self.inner.run_dir.clone(),
        }
    }

    pub(crate) fn matches_record(&self, record: &CatalogRecord) -> bool {
        self.inner.run_id == record.run_id
            && self.inner.created_at == record.created_at
            && self.inner.db_prefix == record.db_prefix
            && self.inner.run_dir == record.run_dir
    }

    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.inner.created_at
    }

    pub(crate) async fn close(&self) -> Result<()> {
        let _guard = self.inner.close_lock.lock().await;
        match self.inner.db.close().await {
            Ok(()) => Ok(()),
            Err(err) if matches!(err.kind(), ErrorKind::Closed(CloseReason::Clean)) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub(crate) async fn snapshot(&self) -> Result<Arc<slatedb::DbSnapshot>> {
        match &self.inner.db {
            SlateRunDb::Writer(db) => Ok(db.snapshot().await?),
            SlateRunDb::Reader(_) => Err(StoreError::ReadOnly),
        }
    }

    pub(crate) async fn validate_init<R>(db: &R, expected: &CatalogRecord) -> Result<bool>
    where
        R: DbRead + Sync,
    {
        match get_json::<R, CatalogRecord>(db, keys::init()).await? {
            Some(existing) if existing == *expected => Ok(true),
            Some(existing) => Err(StoreError::Other(format!(
                "existing _init.json {existing:?} does not match requested catalog {expected:?}"
            ))),
            None => Ok(false),
        }
    }

    pub(crate) async fn build_summary<R>(db: &R, catalog: &CatalogRecord) -> Result<RunSummary>
    where
        R: DbRead + Sync,
    {
        let events = list_events_from(db, 1).await?;
        let state = RunState::apply_events(&events)?;
        Ok(state.build_summary(catalog))
    }

    async fn build_node_snapshot(&self, node: &NodeVisitRef<'_>) -> Result<NodeSnapshot> {
        Ok(NodeSnapshot {
            node_id: node.node_id.to_string(),
            visit: node.visit,
            prompt: self.inner.db.get_text(&keys::node_prompt(node)).await?,
            response: self.inner.db.get_text(&keys::node_response(node)).await?,
            status: self.inner.db.get_json(&keys::node_status(node)).await?,
            outcome: self.inner.db.get_json(&keys::node_outcome(node)).await?,
            provider_used: self
                .inner
                .db
                .get_json(&keys::node_provider_used(node))
                .await?,
            diff: self.inner.db.get_text(&keys::node_diff(node)).await?,
            script_invocation: self
                .inner
                .db
                .get_json(&keys::node_script_invocation(node))
                .await?,
            script_timing: self
                .inner
                .db
                .get_json(&keys::node_script_timing(node))
                .await?,
            parallel_results: self
                .inner
                .db
                .get_json(&keys::node_parallel_results(node))
                .await?,
            stdout: self.inner.db.get_text(&keys::node_stdout(node)).await?,
            stderr: self.inner.db.get_text(&keys::node_stderr(node)).await?,
        })
    }

    async fn projected_state(&self) -> Result<RunState> {
        let next_seq = {
            let cache = self.inner.projection_cache.lock().await;
            cache.last_seq.saturating_add(1)
        };
        let events = self.inner.db.list_events_from(next_seq).await?;
        let mut cache = self.inner.projection_cache.lock().await;
        for event in &events {
            cache.state.apply_event(event)?;
            cache.last_seq = event.seq;
        }
        Ok(cache.state.clone())
    }
}

impl SlateRunStore {
    pub async fn put_node_prompt(&self, node: &NodeVisitRef<'_>, prompt: &str) -> Result<()> {
        self.inner
            .db
            .put_text(&keys::node_prompt(node), prompt)
            .await
    }

    pub async fn put_node_response(&self, node: &NodeVisitRef<'_>, response: &str) -> Result<()> {
        self.inner
            .db
            .put_text(&keys::node_response(node), response)
            .await
    }

    pub async fn put_node_status(
        &self,
        node: &NodeVisitRef<'_>,
        status: &NodeStatusRecord,
    ) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::node_status(node), status)
            .await
    }

    pub async fn put_node_outcome(
        &self,
        node: &NodeVisitRef<'_>,
        outcome: &NodeOutcomeRecord,
    ) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::node_outcome(node), outcome)
            .await
    }

    pub async fn put_node_provider_used(
        &self,
        node: &NodeVisitRef<'_>,
        provider_used: &serde_json::Value,
    ) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::node_provider_used(node), provider_used)
            .await
    }

    pub async fn put_node_diff(&self, node: &NodeVisitRef<'_>, diff: &str) -> Result<()> {
        self.inner.db.put_text(&keys::node_diff(node), diff).await
    }

    pub async fn put_node_script_invocation(
        &self,
        node: &NodeVisitRef<'_>,
        invocation: &serde_json::Value,
    ) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::node_script_invocation(node), invocation)
            .await
    }

    pub async fn put_node_script_timing(
        &self,
        node: &NodeVisitRef<'_>,
        timing: &serde_json::Value,
    ) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::node_script_timing(node), timing)
            .await
    }

    pub async fn put_node_parallel_results(
        &self,
        node: &NodeVisitRef<'_>,
        results: &serde_json::Value,
    ) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::node_parallel_results(node), results)
            .await
    }

    pub async fn put_node_stdout(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()> {
        self.inner.db.put_text(&keys::node_stdout(node), log).await
    }

    pub async fn put_node_stderr(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()> {
        self.inner.db.put_text(&keys::node_stderr(node), log).await
    }

    pub async fn get_node(&self, node: &NodeVisitRef<'_>) -> Result<NodeSnapshot> {
        self.build_node_snapshot(node).await
    }

    pub async fn list_node_visits(&self, node_id: &str) -> Result<Vec<u32>> {
        let prefix = format!("nodes/{node_id}/visit-");
        let mut iter = self.inner.db.scan_prefix(prefix.as_bytes()).await?;
        let mut visits = BTreeSet::new();
        while let Some(entry) = iter.next().await? {
            let key = key_to_string(&entry.key)?;
            if let Some((current_node_id, visit, _)) = keys::parse_node_key(&key) {
                if current_node_id == node_id {
                    visits.insert(visit);
                }
            }
        }
        Ok(visits.into_iter().collect())
    }

    pub async fn list_node_ids(&self) -> Result<Vec<String>> {
        let mut iter = self.inner.db.scan_prefix(b"nodes/").await?;
        let mut node_ids = BTreeSet::new();
        while let Some(entry) = iter.next().await? {
            let key = key_to_string(&entry.key)?;
            if let Some((node_id, _, _)) = keys::parse_node_key(&key) {
                node_ids.insert(node_id);
            }
        }

        let mut asset_iter = self
            .inner
            .db
            .scan_prefix(keys::ARTIFACT_NODES_PREFIX.as_bytes())
            .await?;
        while let Some(entry) = asset_iter.next().await? {
            let key = key_to_string(&entry.key)?;
            if let Some((node_id, _, _)) = keys::parse_node_asset_key(&key) {
                node_ids.insert(node_id);
            }
        }

        Ok(node_ids.into_iter().collect())
    }

    pub async fn reset_for_rewind(&self) -> Result<()> {
        let db = self.inner.db.writer()?;
        for key in [keys::retro_prompt(), keys::retro_response()] {
            db.delete(key).await?;
        }
        for prefix in [
            b"nodes/".as_slice(),
            keys::ARTIFACT_VALUES_PREFIX.as_bytes(),
            keys::ARTIFACT_NODES_PREFIX.as_bytes(),
        ] {
            delete_prefix(db, prefix).await?;
        }
        Ok(())
    }

    pub async fn append_event(&self, payload: &EventPayload) -> Result<u32> {
        payload.validate(&self.inner.run_id)?;
        let seq = self.inner.event_seq.fetch_add(1, Ordering::SeqCst);
        self.inner
            .db
            .put_json(
                &keys::event_key(seq, Utc::now().timestamp_millis()),
                payload,
            )
            .await?;
        Ok(seq)
    }

    pub async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        self.inner.db.list_events_from(1).await
    }

    pub async fn list_events_from(&self, seq: u32) -> Result<Vec<EventEnvelope>> {
        self.inner.db.list_events_from(seq).await
    }

    pub async fn watch_events_from(
        &self,
        seq: u32,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = Result<EventEnvelope>> + Send>>> {
        let inner = Arc::clone(&self.inner);
        let (sender, receiver) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut next_seq = seq;
            loop {
                if sender.is_closed() {
                    return;
                }

                match inner.db.list_events_from(next_seq).await {
                    Ok(events) => {
                        if events.is_empty() {
                            time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                        for event in events {
                            next_seq = event.seq.saturating_add(1);
                            if sender.send(Ok(event)).is_err() {
                                return;
                            }
                        }
                    }
                    Err(err) => {
                        let _ = sender.send(Err(err));
                        return;
                    }
                }
            }
        });

        Ok(Box::pin(UnboundedReceiverStream::new(receiver)))
    }

    pub async fn put_retro_prompt(&self, text: &str) -> Result<()> {
        self.inner.db.put_text(keys::retro_prompt(), text).await
    }

    pub async fn get_retro_prompt(&self) -> Result<Option<String>> {
        self.inner.db.get_text(keys::retro_prompt()).await
    }

    pub async fn put_retro_response(&self, text: &str) -> Result<()> {
        self.inner.db.put_text(keys::retro_response(), text).await
    }

    pub async fn get_retro_response(&self) -> Result<Option<String>> {
        self.inner.db.get_text(keys::retro_response()).await
    }

    pub async fn put_artifact_value(
        &self,
        artifact_id: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::artifact_value(artifact_id), value)
            .await
    }

    pub async fn get_artifact_value(&self, artifact_id: &str) -> Result<Option<serde_json::Value>> {
        self.inner
            .db
            .get_json(&keys::artifact_value(artifact_id))
            .await
    }

    pub async fn list_artifact_values(&self) -> Result<Vec<String>> {
        self.inner.db.list_artifact_values().await
    }

    pub async fn put_asset(
        &self,
        node: &NodeVisitRef<'_>,
        filename: &str,
        data: &[u8],
    ) -> Result<()> {
        self.inner
            .db
            .put_bytes(&keys::node_asset(node, filename), data)
            .await
    }

    pub async fn get_asset(
        &self,
        node: &NodeVisitRef<'_>,
        filename: &str,
    ) -> Result<Option<Bytes>> {
        self.inner
            .db
            .get_bytes(&keys::node_asset(node, filename))
            .await
    }

    pub async fn list_assets(&self, node: &NodeVisitRef<'_>) -> Result<Vec<String>> {
        let prefix = format!("{}/", keys::node_asset_prefix(node));
        let mut iter = self.inner.db.scan_prefix(prefix.as_bytes()).await?;
        let mut assets = Vec::new();
        while let Some(entry) = iter.next().await? {
            let key = key_to_string(&entry.key)?;
            if let Some(asset) = key.strip_prefix(&prefix) {
                assets.push(asset.to_string());
            }
        }
        assets.sort();
        Ok(assets)
    }

    pub async fn list_all_assets(&self) -> Result<Vec<(String, u32, String)>> {
        self.inner.db.list_all_assets().await
    }

    pub async fn state(&self) -> Result<RunState> {
        self.projected_state().await
    }
}

impl SlateRunDb {
    fn writer(&self) -> Result<&slatedb::Db> {
        match self {
            Self::Writer(db) => Ok(db),
            Self::Reader(_) => Err(StoreError::ReadOnly),
        }
    }

    async fn close(&self) -> std::result::Result<(), slatedb::Error> {
        match self {
            Self::Writer(db) => db.close().await,
            Self::Reader(db) => db.close().await,
        }
    }

    async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self {
            Self::Writer(db) => get_json(db, key).await,
            Self::Reader(db) => get_json(db.as_ref(), key).await,
        }
    }

    async fn put_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        put_json(self.writer()?, key, value).await
    }

    async fn get_text(&self, key: &str) -> Result<Option<String>> {
        match self {
            Self::Writer(db) => get_text(db, key).await,
            Self::Reader(db) => get_text(db.as_ref(), key).await,
        }
    }

    async fn put_text(&self, key: &str, value: &str) -> Result<()> {
        put_text(self.writer()?, key, value).await
    }

    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>> {
        match self {
            Self::Writer(db) => get_bytes(db, key).await,
            Self::Reader(db) => db.get(key).await.map_err(Into::into),
        }
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        put_bytes(self.writer()?, key, value).await
    }

    async fn scan_prefix<P>(
        &self,
        prefix: P,
    ) -> std::result::Result<slatedb::DbIterator, slatedb::Error>
    where
        P: AsRef<[u8]> + Send,
    {
        match self {
            Self::Writer(db) => db.scan_prefix(prefix).await,
            Self::Reader(db) => db.scan_prefix(prefix).await,
        }
    }

    async fn list_events_from(&self, start_seq: u32) -> Result<Vec<EventEnvelope>> {
        match self {
            Self::Writer(db) => list_events_from(db, start_seq).await,
            Self::Reader(db) => list_events_from(db.as_ref(), start_seq).await,
        }
    }

    async fn list_artifact_values(&self) -> Result<Vec<String>> {
        match self {
            Self::Writer(db) => list_artifact_values(db).await,
            Self::Reader(db) => list_artifact_values(db.as_ref()).await,
        }
    }

    async fn list_all_assets(&self) -> Result<Vec<(String, u32, String)>> {
        match self {
            Self::Writer(db) => list_all_assets(db).await,
            Self::Reader(db) => list_all_assets(db.as_ref()).await,
        }
    }
}

async fn put_json<T: Serialize>(db: &slatedb::Db, key: &str, value: &T) -> Result<()> {
    db.put(key, serde_json::to_vec(value)?).await?;
    Ok(())
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

async fn put_text(db: &slatedb::Db, key: &str, value: &str) -> Result<()> {
    db.put(key, value.as_bytes()).await?;
    Ok(())
}

async fn get_text<R>(db: &R, key: &str) -> Result<Option<String>>
where
    R: DbRead + Sync,
{
    db.get(key)
        .await?
        .map(|value| {
            String::from_utf8(value.to_vec())
                .map_err(|err| StoreError::Other(format!("stored text is not valid UTF-8: {err}")))
        })
        .transpose()
}

async fn put_bytes(db: &slatedb::Db, key: &str, value: &[u8]) -> Result<()> {
    db.put(key, value).await?;
    Ok(())
}

async fn delete_prefix(db: &slatedb::Db, prefix: &[u8]) -> Result<()> {
    let mut iter = db.scan_prefix(prefix).await?;
    let mut keys = Vec::new();
    while let Some(entry) = iter.next().await? {
        keys.push(key_to_string(&entry.key)?);
    }
    for key in keys {
        db.delete(key).await?;
    }
    Ok(())
}

async fn get_bytes(db: &slatedb::Db, key: &str) -> Result<Option<Bytes>> {
    Ok(db.get(key).await?)
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

async fn list_events_from<R>(db: &R, start_seq: u32) -> Result<Vec<EventEnvelope>>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(keys::EVENTS_PREFIX.as_bytes()).await?;
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

async fn list_artifact_values<R>(db: &R) -> Result<Vec<String>>
where
    R: DbRead + Sync,
{
    let mut iter = db
        .scan_prefix(keys::ARTIFACT_VALUES_PREFIX.as_bytes())
        .await?;
    let mut artifact_ids = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some(artifact_id) = keys::parse_artifact_value_id(&key) else {
            continue;
        };
        artifact_ids.push(artifact_id);
    }
    artifact_ids.sort();
    Ok(artifact_ids)
}

async fn list_all_assets<R>(db: &R) -> Result<Vec<(String, u32, String)>>
where
    R: DbRead + Sync,
{
    let mut iter = db
        .scan_prefix(keys::ARTIFACT_NODES_PREFIX.as_bytes())
        .await?;
    let mut assets = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some(asset) = keys::parse_node_asset_key(&key) else {
            continue;
        };
        assets.push(asset);
    }
    assets.sort();
    Ok(assets)
}

fn key_to_string(key: &Bytes) -> Result<String> {
    String::from_utf8(key.to_vec())
        .map_err(|err| StoreError::Other(format!("stored key is not valid UTF-8: {err}")))
}
