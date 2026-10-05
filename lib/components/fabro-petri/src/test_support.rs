//! Petri's test kit, for Fabro crates that check a store implementation
//! against Petri's contract from their own tests; an in-memory platform
//! record store and an in-memory blob table for tests of the hooks and
//! recovery; and the readers over the view tables a test compares a live
//! view with. Compiled only with the `test-support` feature, which a
//! dev-dependency turns on.

pub mod finalization;

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use fabro_db::DbPool;
use fabro_store::platform_records::{PlatformRecordStore, now_ms};
use fabro_store::{
    PlatformRecord, PlatformRecordKind, RunProjection, StagePosition, StoredPlatformRecord,
};
use fabro_types::{BlobHash, RunId};
use fabro_util::error::collect_chain;
use fabro_util::sync;
use petri_execution::events::{self, RunEvent};
use petri_execution::{Access, CoordinatorEvent, RunKey, RunStore as _};
use petri_store::StoreError;
pub use petri_testkit::run_store;
use tracing::warn;

use crate::SqliteRunStore;
use crate::blobs::Blobs;
use crate::platform_records::{PlatformRecordError, PlatformRecords};
use crate::projection::RunView;
use crate::projector::{self, Positions, ProjectError, Projector, order, stream};

/// Whether the projector keeps a cache for the run: the replay and the
/// view its passes continue from.
#[must_use]
pub fn cache_held(projector: &Projector, run_id: RunId) -> bool {
    projector.caches.holds(run_id)
}

/// Drop the projector's caches not used for `idle`, as its passes do
/// after the documented idle period; how many were dropped.
pub fn drop_idle_caches(projector: &Projector, idle: Duration) -> usize {
    projector.caches.sweep(idle)
}

/// A blob table in memory.
#[derive(Debug, Default)]
pub struct MemoryBlobs {
    rows: Mutex<HashMap<BlobHash, Vec<u8>>>,
}

impl MemoryBlobs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many blobs the table holds.
    #[must_use]
    pub fn len(&self) -> usize {
        sync::lock(&self.rows).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl Blobs for MemoryBlobs {
    async fn write(&self, bytes: &[u8]) -> anyhow::Result<BlobHash> {
        let hash = BlobHash::new(bytes);
        sync::lock(&self.rows).insert(hash, bytes.to_vec());
        Ok(hash)
    }

    async fn read(&self, hash: &BlobHash) -> anyhow::Result<Option<Bytes>> {
        Ok(sync::lock(&self.rows)
            .get(hash)
            .map(|bytes| Bytes::copy_from_slice(bytes)))
    }
}

/// Platform records kept in memory, per run, in seq order.
#[derive(Debug, Default)]
pub struct MemoryPlatformRecords {
    runs: Mutex<HashMap<RunId, Vec<StoredPlatformRecord>>>,
}

impl MemoryPlatformRecords {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every record of the run, in seq order.
    #[must_use]
    pub fn records(&self, run_id: &RunId) -> Vec<StoredPlatformRecord> {
        sync::lock(&self.runs)
            .get(run_id)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl PlatformRecords for MemoryPlatformRecords {
    async fn append(
        &self,
        run_id: &RunId,
        record: &PlatformRecord,
        position: Option<StagePosition>,
    ) -> Result<StoredPlatformRecord, PlatformRecordError> {
        let mut runs = sync::lock(&self.runs);
        let records = runs.entry(*run_id).or_default();
        let stored = StoredPlatformRecord {
            seq: records.len() as u64 + 1,
            recorded_at: now_ms(),
            record: record.clone(),
            position,
        };
        records.push(stored.clone());
        Ok(stored)
    }

    async fn read_kind(
        &self,
        run_id: &RunId,
        kind: PlatformRecordKind,
    ) -> Result<Vec<StoredPlatformRecord>, PlatformRecordError> {
        Ok(self
            .records(run_id)
            .into_iter()
            .filter(|record| record.record.kind() == kind)
            .collect())
    }
}

/// The run's projection rebuilt from its records alone, with nothing
/// stored: what a fresh projector would commit over the same records. A test
/// compares it with the live view. `records` and `views` are the two pools
/// [`Projector::new`] takes.
pub async fn rebuild(
    records: &DbPool,
    views: &DbPool,
    run_id: RunId,
) -> Result<(Option<RunProjection>, Positions, u64), ProjectError> {
    let store = SqliteRunStore::new(records.clone());
    let platform = PlatformRecordStore::new(views.clone());
    let key = RunKey::new(run_id.to_string());
    let platform_records = platform.read(&run_id).await.map_err(ProjectError::Store)?;
    let events = match store.open(&key, Access::Read).await {
        Ok(logs) => events::replay_run(&*logs)
            .await
            .inspect_err(|error| {
                warn!(error = %collect_chain(error).join(": "), "rebuild: the run does not replay");
            })
            .unwrap_or_default(),
        Err(StoreError::NotFound { .. }) => Vec::new(),
        Err(error) => return Err(ProjectError::Open(error)),
    };
    let run_finished = events.iter().any(|event| {
        matches!(
            event.coordinator(),
            Some(CoordinatorEvent::RunFinished { .. })
        )
    });
    let (items, _held) =
        order::order_items(&events, &platform_records, &BTreeSet::new(), run_finished);
    let mut view = RunView::new();
    let mut positions = Positions::default();
    let mut stream_seq = 0;
    stream::stream_rows(&items, &mut view, &mut positions, &mut stream_seq)?;
    Ok((view.projection, positions, stream_seq))
}

/// The stored view's positions and stream sequence; `views` is the pool
/// the view tables live in.
pub async fn stored_positions(
    views: &DbPool,
    run_id: RunId,
) -> Result<Option<(Positions, u64)>, ProjectError> {
    projector::stored_positions(views, run_id).await
}

/// The stored view's projection, for a test or a reader outside the store.
pub async fn stored_projection(
    views: &DbPool,
    run_id: RunId,
) -> Result<Option<RunProjection>, ProjectError> {
    let json: Option<String> =
        sqlx::query_scalar("SELECT projection_json FROM petri_projection WHERE run_id = ?")
            .bind(run_id.to_string())
            .fetch_optional(views)
            .await
            .map_err(ProjectError::Database)?;
    json.map(|json| serde_json::from_str(&json).map_err(ProjectError::Encode))
        .transpose()
}

/// The stream rows of a run: `(stream_seq, item_kind, item_id)`, in order.
pub async fn stored_stream(
    views: &DbPool,
    run_id: RunId,
) -> Result<Vec<(u64, String, String)>, ProjectError> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT stream_seq, item_kind, item_id FROM petri_stream WHERE run_id = ? ORDER BY stream_seq",
    )
    .bind(run_id.to_string())
    .fetch_all(views)
    .await
    .map_err(ProjectError::Database)?;
    Ok(rows
        .into_iter()
        .map(|(seq, kind, id)| (u64::try_from(seq).unwrap_or(0), kind, id))
        .collect())
}

/// Every stored platform record of a run, for a reader outside the store.
pub async fn stored_platform_records(
    views: &DbPool,
    run_id: RunId,
) -> Result<Vec<StoredPlatformRecord>, ProjectError> {
    PlatformRecordStore::new(views.clone())
        .read(&run_id)
        .await
        .map_err(ProjectError::Store)
}

/// A recorded event's projection is what `RunEvent` serializes to.
#[must_use]
pub fn event_json(event: &RunEvent) -> serde_json::Value {
    serde_json::to_value(event).unwrap_or_default()
}
