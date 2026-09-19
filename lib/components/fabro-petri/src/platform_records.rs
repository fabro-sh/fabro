//! Fabro's platform records as a Petri run's adapters reach them.
//!
//! The records themselves are `fabro_store::platform_records`: one table
//! beside Petri's records, one typed enum of kinds. What differs is where
//! the adapter runs. In the server process (the in-process test path, and
//! startup recovery) the table is reached directly, through
//! [`SqlitePlatformRecords`]; in a run's worker process it is reached over
//! the server's API with the worker's token, through
//! [`HttpPlatformRecords`], as the run's Petri records are. Both answer
//! the one [`PlatformRecords`] interface the hooks and recovery use.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_api::types::{PetriPlatformRecord, PetriPlatformRecordAppendRequest};
use fabro_client::Client;
use fabro_store::{
    PlatformRecord, PlatformRecordKind, PlatformRecordStore, RunSummaryStore, StagePosition,
    StoredPlatformRecord,
};
use fabro_types::RunId;
use serde_json::Value;

/// Why a platform record could not be stored or read.
#[derive(Debug, thiserror::Error)]
pub enum PlatformRecordError {
    #[error("the platform record store failed")]
    Store(#[source] fabro_store::Error),
    #[error("the platform record request to the server failed")]
    Api(#[source] anyhow::Error),
    #[error("the platform record does not encode as JSON")]
    Encode(#[source] serde_json::Error),
}

/// The platform records of a run, wherever the adapter runs.
#[async_trait]
pub trait PlatformRecords: Send + Sync {
    /// Store a record at the run's next seq, tied to a Petri stage when it
    /// belongs to one.
    async fn append(
        &self,
        run_id: &RunId,
        record: &PlatformRecord,
        position: Option<StagePosition>,
    ) -> Result<StoredPlatformRecord, PlatformRecordError>;

    /// The run's records of one kind, in seq order.
    async fn read_kind(
        &self,
        run_id: &RunId,
        kind: PlatformRecordKind,
    ) -> Result<Vec<StoredPlatformRecord>, PlatformRecordError>;
}

/// The table in the server's database, with the projector's wake-up after
/// each append.
#[derive(Clone)]
pub struct SqlitePlatformRecords {
    store:     PlatformRecordStore,
    summaries: Arc<RunSummaryStore>,
}

impl fmt::Debug for SqlitePlatformRecords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlitePlatformRecords")
            .finish_non_exhaustive()
    }
}

impl SqlitePlatformRecords {
    #[must_use]
    pub fn new(summaries: Arc<RunSummaryStore>) -> Self {
        Self {
            store: summaries.platform_records(),
            summaries,
        }
    }
}

#[async_trait]
impl PlatformRecords for SqlitePlatformRecords {
    async fn append(
        &self,
        run_id: &RunId,
        record: &PlatformRecord,
        position: Option<StagePosition>,
    ) -> Result<StoredPlatformRecord, PlatformRecordError> {
        let stored = self
            .store
            .append(run_id, record, position)
            .await
            .map_err(PlatformRecordError::Store)?;
        self.summaries.notify_platform_record(*run_id);
        Ok(stored)
    }

    async fn read_kind(
        &self,
        run_id: &RunId,
        kind: PlatformRecordKind,
    ) -> Result<Vec<StoredPlatformRecord>, PlatformRecordError> {
        self.store
            .read_kind(run_id, kind)
            .await
            .map_err(PlatformRecordError::Store)
    }
}

/// The table as a run's worker reaches it: the server's
/// `/api/v1/runs/{id}/petri/platform-records` endpoints with the worker's
/// token.
pub struct HttpPlatformRecords {
    client: Client,
}

impl fmt::Debug for HttpPlatformRecords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpPlatformRecords")
            .field("server", &self.client.base_url())
            .finish_non_exhaustive()
    }
}

impl HttpPlatformRecords {
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl PlatformRecords for HttpPlatformRecords {
    async fn append(
        &self,
        run_id: &RunId,
        record: &PlatformRecord,
        position: Option<StagePosition>,
    ) -> Result<StoredPlatformRecord, PlatformRecordError> {
        let Value::Object(record) =
            serde_json::to_value(record).map_err(PlatformRecordError::Encode)?
        else {
            return Err(PlatformRecordError::Api(anyhow::anyhow!(
                "a platform record encodes as a JSON object"
            )));
        };
        let body = PetriPlatformRecordAppendRequest {
            record,
            execution: position.map(|position| position.execution),
            firing: position.map(|position| position.firing),
        };
        let stored = self
            .client
            .append_petri_platform_record(run_id, body)
            .await
            .map_err(PlatformRecordError::Api)?;
        decode(stored)
    }

    async fn read_kind(
        &self,
        run_id: &RunId,
        kind: PlatformRecordKind,
    ) -> Result<Vec<StoredPlatformRecord>, PlatformRecordError> {
        let records = self
            .client
            .list_petri_platform_records(run_id, Some(&kind.to_string()))
            .await
            .map_err(PlatformRecordError::Api)?;
        records.into_iter().map(decode).collect()
    }
}

/// A wire record back into the store's shape.
fn decode(wire: PetriPlatformRecord) -> Result<StoredPlatformRecord, PlatformRecordError> {
    let record: PlatformRecord =
        serde_json::from_value(Value::Object(wire.record)).map_err(PlatformRecordError::Encode)?;
    let position = match (wire.execution, wire.firing) {
        (Some(execution), Some(firing)) => Some(StagePosition { execution, firing }),
        _ => None,
    };
    Ok(StoredPlatformRecord {
        seq: wire.seq,
        recorded_at: wire.recorded_at,
        record,
        position,
    })
}
