use chrono::{DateTime, Utc};

mod artifact_store;
mod error;
mod keyed_mutex;
mod keys;
mod record;
mod run_state;
mod serializable_projection;
mod slate;
mod types;

pub use artifact_store::{ArtifactStore, NodeArtifact, stage_storage_segment};
pub use error::{Error, Result};
pub use fabro_types::{
    EventEnvelope, PendingInterviewRecord, RunBlobId, RunProjection, RunSummary, StageId,
    StageProjection,
};
pub(crate) use keyed_mutex::KeyedMutex;
pub use run_state::RunProjectionReducer;
pub use serializable_projection::SerializableProjection;
pub use slate::{
    AuthCode, AuthCodeStore, Blob, BlobStore, ConsumeOutcome, Database, RefreshToken,
    RefreshTokenStore, RunCatalogIndex, RunDatabase, Runs,
};
pub use types::EventPayload;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListRunsQuery {
    pub start: Option<DateTime<Utc>>,
    pub end:   Option<DateTime<Utc>>,
}
