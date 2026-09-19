//! Petri's test kit, for Fabro crates that check a store implementation
//! against Petri's contract from their own tests, an in-memory platform
//! record store and an in-memory blob table for tests of the hooks and
//! recovery. Compiled only with the `test-support` feature, which a
//! dev-dependency turns on.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use fabro_store::platform_records::now_ms;
use fabro_store::{PlatformRecord, PlatformRecordKind, StagePosition, StoredPlatformRecord};
use fabro_types::{BlobHash, RunId};
pub use petri_testkit::run_store;

use crate::blobs::Blobs;
use crate::platform_records::{PlatformRecordError, PlatformRecords};
use crate::projector::Projector;

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
        lock(&self.rows).len()
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
        lock(&self.rows).insert(hash, bytes.to_vec());
        Ok(hash)
    }

    async fn read(&self, hash: &BlobHash) -> anyhow::Result<Option<Bytes>> {
        Ok(lock(&self.rows)
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
        lock(&self.runs).get(run_id).cloned().unwrap_or_default()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[async_trait]
impl PlatformRecords for MemoryPlatformRecords {
    async fn append(
        &self,
        run_id: &RunId,
        record: &PlatformRecord,
        position: Option<StagePosition>,
    ) -> Result<StoredPlatformRecord, PlatformRecordError> {
        let mut runs = lock(&self.runs);
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
