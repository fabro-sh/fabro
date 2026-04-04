use std::sync::Arc;

use chrono::{DateTime, Utc};

mod error;
mod keys;
mod run_state;
mod runtime;
mod slate;
mod types;

pub use error::{Result, StoreError};
pub use fabro_types::StageId;
pub use run_state::{NodeState, RunProjection};
pub use runtime::RuntimeState;
pub use slate::{NodeAsset, SlateRunStore, SlateStore};
pub use types::{CatalogRecord, EventEnvelope, EventPayload, RunSummary};

pub type StoreHandle = Arc<SlateStore>;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ListRunsQuery {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}
