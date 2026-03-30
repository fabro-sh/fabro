use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
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
use crate::{
    CatalogRecord, EventEnvelope, EventPayload, NodeSnapshot, NodeVisitRef, Result, RunSnapshot,
    RunStore, RunSummary, StoreError,
};
use fabro_types::{
    Checkpoint, Conclusion, NodeStatusRecord, Retro, RunId, RunRecord, RunStatusRecord,
    SandboxRecord, StartRecord,
};

#[derive(Clone)]
pub(crate) struct SlateRunStore {
    inner: Arc<SlateRunStoreInner>,
}

pub(crate) struct SlateRunStoreInner {
    run_id: RunId,
    created_at: DateTime<Utc>,
    db_prefix: String,
    run_dir: Option<String>,
    db: SlateRunDb,
    event_seq: AtomicU32,
    checkpoint_seq: AtomicU32,
    close_lock: Mutex<()>,
}

enum SlateRunDb {
    Writer(slatedb::Db),
    Reader(Box<DbReader>),
}

impl SlateRunStore {
    pub(crate) async fn open_writer(record: CatalogRecord, db: slatedb::Db) -> Result<Self> {
        let event_seq = recover_next_seq(&db, keys::EVENTS_PREFIX, keys::parse_event_seq).await?;
        let checkpoint_seq =
            recover_next_seq(&db, keys::CHECKPOINTS_PREFIX, keys::parse_checkpoint_seq).await?;
        Ok(Self {
            inner: Arc::new(SlateRunStoreInner {
                run_id: record.run_id,
                created_at: record.created_at,
                db_prefix: record.db_prefix,
                run_dir: record.run_dir,
                db: SlateRunDb::Writer(db),
                event_seq: AtomicU32::new(event_seq),
                checkpoint_seq: AtomicU32::new(checkpoint_seq),
                close_lock: Mutex::new(()),
            }),
        })
    }

    pub(crate) async fn open_reader(record: CatalogRecord, db: DbReader) -> Result<Self> {
        let event_seq = recover_next_seq(&db, keys::EVENTS_PREFIX, keys::parse_event_seq).await?;
        let checkpoint_seq =
            recover_next_seq(&db, keys::CHECKPOINTS_PREFIX, keys::parse_checkpoint_seq).await?;
        Ok(Self {
            inner: Arc::new(SlateRunStoreInner {
                run_id: record.run_id,
                created_at: record.created_at,
                db_prefix: record.db_prefix,
                run_dir: record.run_dir,
                db: SlateRunDb::Reader(Box::new(db)),
                event_seq: AtomicU32::new(event_seq),
                checkpoint_seq: AtomicU32::new(checkpoint_seq),
                close_lock: Mutex::new(()),
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
        let run = get_json::<_, RunRecord>(db, keys::run()).await?;
        let start = get_json::<_, StartRecord>(db, keys::start()).await?;
        let status = get_json::<_, RunStatusRecord>(db, keys::status()).await?;
        let conclusion = get_json::<_, Conclusion>(db, keys::conclusion()).await?;

        let workflow_name = run.as_ref().map(|run| {
            if run.graph.name.is_empty() {
                "unnamed".to_string()
            } else {
                run.graph.name.clone()
            }
        });
        let goal = run.as_ref().and_then(|run| {
            let goal = run.graph.goal();
            (!goal.is_empty()).then(|| goal.to_string())
        });

        Ok(RunSummary {
            run_id: catalog.run_id,
            created_at: catalog.created_at,
            db_prefix: catalog.db_prefix.clone(),
            run_dir: catalog.run_dir.clone(),
            workflow_name,
            workflow_slug: run.as_ref().and_then(|run| run.workflow_slug.clone()),
            goal,
            labels: run
                .as_ref()
                .map(|run| run.labels.clone())
                .unwrap_or_default(),
            host_repo_path: run.as_ref().and_then(|run| run.host_repo_path.clone()),
            start_time: start.map(|start| start.start_time),
            status: status.as_ref().map(|status| status.status),
            status_reason: status.and_then(|status| status.reason),
            duration_ms: conclusion.as_ref().map(|conclusion| conclusion.duration_ms),
            total_cost: conclusion.and_then(|conclusion| conclusion.total_cost),
        })
    }

    fn validate_run_record(&self, record: &RunRecord) -> Result<()> {
        if record.created_at != self.inner.created_at {
            return Err(StoreError::Other(format!(
                "run record created_at {:?} does not match store created_at {:?}",
                record.created_at, self.inner.created_at
            )));
        }
        if record.run_id != self.inner.run_id {
            return Err(StoreError::Other(format!(
                "run record run_id {:?} does not match store run_id {:?}",
                record.run_id, self.inner.run_id
            )));
        }
        Ok(())
    }

    async fn build_node_snapshot(&self, node: &NodeVisitRef<'_>) -> Result<NodeSnapshot> {
        Ok(NodeSnapshot {
            node_id: node.node_id.to_string(),
            visit: node.visit,
            prompt: self.inner.db.get_text(&keys::node_prompt(node)).await?,
            response: self.inner.db.get_text(&keys::node_response(node)).await?,
            status: self.inner.db.get_json(&keys::node_status(node)).await?,
            stdout: self.inner.db.get_text(&keys::node_stdout(node)).await?,
            stderr: self.inner.db.get_text(&keys::node_stderr(node)).await?,
        })
    }
}

#[async_trait]
impl RunStore for SlateRunStore {
    async fn put_run(&self, record: &RunRecord) -> Result<()> {
        self.validate_run_record(record)?;
        self.inner.db.put_json(keys::run(), record).await
    }

    async fn get_run(&self) -> Result<Option<RunRecord>> {
        self.inner.db.get_json(keys::run()).await
    }

    async fn put_start(&self, record: &StartRecord) -> Result<()> {
        self.inner.db.put_json(keys::start(), record).await
    }

    async fn get_start(&self) -> Result<Option<StartRecord>> {
        self.inner.db.get_json(keys::start()).await
    }

    async fn put_status(&self, record: &RunStatusRecord) -> Result<()> {
        self.inner.db.put_json(keys::status(), record).await
    }

    async fn get_status(&self) -> Result<Option<RunStatusRecord>> {
        self.inner.db.get_json(keys::status()).await
    }

    async fn put_checkpoint(&self, record: &Checkpoint) -> Result<()> {
        self.inner.db.put_json(keys::checkpoint(), record).await
    }

    async fn get_checkpoint(&self) -> Result<Option<Checkpoint>> {
        self.inner.db.get_json(keys::checkpoint()).await
    }

    async fn append_checkpoint(&self, record: &Checkpoint) -> Result<u32> {
        let seq = self.inner.checkpoint_seq.fetch_add(1, Ordering::SeqCst);
        self.put_checkpoint(record).await?;
        self.inner
            .db
            .put_json(
                &keys::checkpoint_history_key(seq, Utc::now().timestamp_millis()),
                record,
            )
            .await?;
        Ok(seq)
    }

    async fn list_checkpoints(&self) -> Result<Vec<(u32, Checkpoint)>> {
        self.inner.db.list_checkpoints().await
    }

    async fn put_conclusion(&self, record: &Conclusion) -> Result<()> {
        self.inner.db.put_json(keys::conclusion(), record).await
    }

    async fn get_conclusion(&self) -> Result<Option<Conclusion>> {
        self.inner.db.get_json(keys::conclusion()).await
    }

    async fn put_retro(&self, retro: &Retro) -> Result<()> {
        self.inner.db.put_json(keys::retro(), retro).await
    }

    async fn get_retro(&self) -> Result<Option<Retro>> {
        self.inner.db.get_json(keys::retro()).await
    }

    async fn put_graph(&self, dot_source: &str) -> Result<()> {
        self.inner.db.put_text(keys::graph(), dot_source).await
    }

    async fn get_graph(&self) -> Result<Option<String>> {
        self.inner.db.get_text(keys::graph()).await
    }

    async fn put_sandbox(&self, record: &SandboxRecord) -> Result<()> {
        self.inner.db.put_json(keys::sandbox(), record).await
    }

    async fn get_sandbox(&self) -> Result<Option<SandboxRecord>> {
        self.inner.db.get_json(keys::sandbox()).await
    }

    async fn put_node_prompt(&self, node: &NodeVisitRef<'_>, prompt: &str) -> Result<()> {
        self.inner
            .db
            .put_text(&keys::node_prompt(node), prompt)
            .await
    }

    async fn put_node_response(&self, node: &NodeVisitRef<'_>, response: &str) -> Result<()> {
        self.inner
            .db
            .put_text(&keys::node_response(node), response)
            .await
    }

    async fn put_node_status(
        &self,
        node: &NodeVisitRef<'_>,
        status: &NodeStatusRecord,
    ) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::node_status(node), status)
            .await
    }

    async fn put_node_stdout(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()> {
        self.inner.db.put_text(&keys::node_stdout(node), log).await
    }

    async fn put_node_stderr(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()> {
        self.inner.db.put_text(&keys::node_stderr(node), log).await
    }

    async fn get_node(&self, node: &NodeVisitRef<'_>) -> Result<NodeSnapshot> {
        self.build_node_snapshot(node).await
    }

    async fn list_node_visits(&self, node_id: &str) -> Result<Vec<u32>> {
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

    async fn append_event(&self, payload: &EventPayload) -> Result<u32> {
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

    async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        self.inner.db.list_events_from(1).await
    }

    async fn list_events_from(&self, seq: u32) -> Result<Vec<EventEnvelope>> {
        self.inner.db.list_events_from(seq).await
    }

    async fn watch_events_from(
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

    async fn put_retro_prompt(&self, text: &str) -> Result<()> {
        self.inner.db.put_text(keys::retro_prompt(), text).await
    }

    async fn get_retro_prompt(&self) -> Result<Option<String>> {
        self.inner.db.get_text(keys::retro_prompt()).await
    }

    async fn put_retro_response(&self, text: &str) -> Result<()> {
        self.inner.db.put_text(keys::retro_response(), text).await
    }

    async fn get_retro_response(&self) -> Result<Option<String>> {
        self.inner.db.get_text(keys::retro_response()).await
    }

    async fn put_artifact_value(&self, artifact_id: &str, value: &serde_json::Value) -> Result<()> {
        self.inner
            .db
            .put_json(&keys::artifact_value(artifact_id), value)
            .await
    }

    async fn get_artifact_value(&self, artifact_id: &str) -> Result<Option<serde_json::Value>> {
        self.inner
            .db
            .get_json(&keys::artifact_value(artifact_id))
            .await
    }

    async fn list_artifact_values(&self) -> Result<Vec<String>> {
        self.inner.db.list_artifact_values().await
    }

    async fn put_asset(&self, node: &NodeVisitRef<'_>, filename: &str, data: &[u8]) -> Result<()> {
        self.inner
            .db
            .put_bytes(&keys::node_asset(node, filename), data)
            .await
    }

    async fn get_asset(&self, node: &NodeVisitRef<'_>, filename: &str) -> Result<Option<Bytes>> {
        self.inner
            .db
            .get_bytes(&keys::node_asset(node, filename))
            .await
    }

    async fn list_assets(&self, node: &NodeVisitRef<'_>) -> Result<Vec<String>> {
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

    async fn list_all_assets(&self) -> Result<Vec<(String, u32, String)>> {
        self.inner.db.list_all_assets().await
    }

    async fn get_snapshot(&self) -> Result<Option<RunSnapshot>> {
        let Some(run) = self.get_run().await? else {
            return Ok(None);
        };

        let mut iter = self.inner.db.scan_prefix(b"nodes/").await?;
        let mut visits = BTreeSet::new();
        while let Some(entry) = iter.next().await? {
            let key = key_to_string(&entry.key)?;
            if let Some((node_id, visit, _)) = keys::parse_node_key(&key) {
                visits.insert((node_id, visit));
            }
        }

        let mut nodes = Vec::new();
        for (node_id, visit) in visits {
            let node = NodeVisitRef {
                node_id: &node_id,
                visit,
            };
            nodes.push(self.build_node_snapshot(&node).await?);
        }

        Ok(Some(RunSnapshot {
            run,
            start: self.get_start().await?,
            status: self.get_status().await?,
            checkpoint: self.get_checkpoint().await?,
            conclusion: self.get_conclusion().await?,
            retro: self.get_retro().await?,
            graph: self.get_graph().await?,
            sandbox: self.get_sandbox().await?,
            nodes,
        }))
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

    async fn list_checkpoints(&self) -> Result<Vec<(u32, Checkpoint)>> {
        match self {
            Self::Writer(db) => list_checkpoints(db).await,
            Self::Reader(db) => list_checkpoints(db.as_ref()).await,
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

async fn list_checkpoints<R>(db: &R) -> Result<Vec<(u32, Checkpoint)>>
where
    R: DbRead + Sync,
{
    let mut iter = db.scan_prefix(keys::CHECKPOINTS_PREFIX.as_bytes()).await?;
    let mut checkpoints = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = key_to_string(&entry.key)?;
        let Some(seq) = keys::parse_checkpoint_seq(&key) else {
            continue;
        };
        checkpoints.push((seq, serde_json::from_slice(&entry.value)?));
    }
    checkpoints.sort_by_key(|(seq, _)| *seq);
    Ok(checkpoints)
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
