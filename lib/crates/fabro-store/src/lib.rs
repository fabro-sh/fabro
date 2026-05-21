use chrono::{DateTime, Utc};

mod artifact_store;
mod error;
mod keyed_mutex;
mod keys;
mod record;
mod run_sessions;
mod run_state;
mod serializable_projection;
mod slate;
mod types;

pub use artifact_store::{
    ArtifactKey, ArtifactStore, NodeArtifact, StageArtifactEntry, retry_storage_segment,
    stage_storage_segment,
};
pub use error::{Error, Result};
pub use fabro_types::{
    EventEnvelope, PendingInterviewRecord, Run, RunBlobId, RunProjection, StageId, StageProjection,
};
pub(crate) use keyed_mutex::KeyedMutex;
pub use run_sessions::{
    ProjectedRunSession, project_run_session, project_run_session_with_context,
    project_run_sessions,
};
pub use run_state::RunProjectionReducer;
pub use serializable_projection::SerializableProjection;
pub use slate::{
    AuthCode, AuthCodeStore, Blob, BlobStore, CachedRunProjection, ConsumeOutcome, Database,
    RefreshToken, RefreshTokenStore, RunCatalogIndex, RunDatabase, Runs, UnreadableRun,
};
pub use types::EventPayload;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListRunsQuery {
    pub start:     Option<DateTime<Utc>>,
    pub end:       Option<DateTime<Utc>>,
    pub parent_id: Option<fabro_types::RunId>,
}
