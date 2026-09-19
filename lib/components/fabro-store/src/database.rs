//! The run store the server holds: the blob table and the run summary
//! store over one SQLite pool, behind one handle.
//!
//! A run's history is Petri's records (`petri_records`) and Fabro's
//! platform records; its view is the projection the projector commits.
//! This handle carries the two stores every reader of a run reaches, and
//! the run-level operations that span them.

use std::sync::Arc;

use fabro_types::{RunId, RunProjection};

use crate::{BlobStore, PlatformRecordHook, Result, RunSummaryStore};

#[derive(Clone)]
pub struct Database {
    blobs:             Arc<BlobStore>,
    run_summary_store: Arc<RunSummaryStore>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}

impl Database {
    #[must_use]
    pub fn new(blobs: Arc<BlobStore>, run_summary_store: Arc<RunSummaryStore>) -> Self {
        Self {
            blobs,
            run_summary_store,
        }
    }

    #[must_use]
    pub fn run_summary_store(&self) -> Arc<RunSummaryStore> {
        Arc::clone(&self.run_summary_store)
    }

    #[must_use]
    pub fn blobs(&self) -> Arc<BlobStore> {
        Arc::clone(&self.blobs)
    }

    /// The run's projection: the one its projector last committed over
    /// Petri's records and the platform records, or `None` before the
    /// first view pass commits (or for no such run).
    pub async fn load_run_projection(&self, run_id: &RunId) -> Result<Option<Arc<RunProjection>>> {
        self.run_summary_store.load_petri_projection(run_id).await
    }

    /// Install the wake-up called after a platform record of a run is
    /// committed.
    pub fn set_platform_record_hook(&self, hook: PlatformRecordHook) {
        self.run_summary_store.set_platform_record_hook(hook);
    }

    /// Forget the run's row. Its Petri records, platform records and view
    /// are the projector's to delete; its blobs are content-addressed and
    /// shared.
    pub async fn delete_run(&self, run_id: &RunId) -> Result<()> {
        self.run_summary_store.delete_canonical(run_id).await
    }
}
