use chrono::{DateTime, Utc};

mod artifact_store;
mod error;
mod keys;
mod run_state;
mod slate;
mod types;

pub use artifact_store::{ArtifactStore, NodeArtifact};
pub use error::{Result, StoreError};
pub use fabro_types::{RunBlobId, StageId};
pub use run_state::{NodeState, PendingInterviewRecord, RunProjection};
pub use slate::{Database, RunDatabase, Runs};
pub use types::{EventEnvelope, EventPayload, RunSummary};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListRunsQuery {
    pub start: Option<DateTime<Utc>>,
    pub end:   Option<DateTime<Utc>>,
}
