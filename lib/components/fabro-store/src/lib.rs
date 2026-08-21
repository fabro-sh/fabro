use chrono::{DateTime, Utc};

mod artifact_store;
mod blob_store;
mod error;
mod keyed_mutex;
mod keys;
mod record;
mod run_sessions;
mod run_state;
mod run_summary_store;
mod serializable_projection;
mod slate;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod types;

pub use artifact_store::{
    ArtifactKey, ArtifactStore, NodeArtifact, StageArtifactEntry, retry_storage_segment,
    stage_storage_segment,
};
pub use blob_store::{Blob, BlobStore};
pub use error::{Error, Result};
pub use fabro_types::{
    BlobHash, EventEnvelope, PendingInterviewRecord, Run, RunProjection, StageId, StageProjection,
};
pub use keyed_mutex::{KeyedMutex, KeyedMutexGuard};
pub use run_sessions::{
    ProjectedRunSession, project_run_session, project_run_session_with_context,
    project_run_sessions,
};
pub use run_state::RunProjectionReducer;
pub use run_summary_store::{
    RunSummaryIdentity, RunSummaryListQuery, RunSummaryPage, RunSummarySort,
    RunSummarySortDirection, RunSummaryStore, RunSummaryVisibility,
};
pub use serializable_projection::SerializableProjection;
pub use slate::{
    AuthCode, AuthCodeStore, CachedRunProjection, ConsumeOutcome, Database, MainEventStore,
    RefreshToken, RefreshTokenStore, RunCatalogIndex, RunDatabase, Runs, UnreadableRun,
};
pub use types::EventPayload;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListRunsQuery {
    pub start:     Option<DateTime<Utc>>,
    pub end:       Option<DateTime<Utc>>,
    pub parent_id: Option<fabro_types::RunId>,
}
