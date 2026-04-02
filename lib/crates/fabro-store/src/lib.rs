use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::Stream;

mod error;
mod keys;
mod memory;
mod runtime;
mod slate;
mod types;

pub use error::{Result, StoreError};
pub use memory::InMemoryStore;
pub use runtime::RuntimeState;
pub use slate::SlateStore;
pub use types::{
    CatalogRecord, EventEnvelope, EventPayload, NodeSnapshot, NodeVisitRef, RunSnapshot, RunSummary,
};

use fabro_types::{
    Checkpoint, Conclusion, NodeStatusRecord, Outcome, PullRequestRecord, Retro, RunId, RunRecord,
    RunStatusRecord, SandboxRecord, StageUsage, StartRecord,
};

pub type NodeOutcomeRecord = Outcome<Option<StageUsage>>;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListRunsQuery {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn create_run(
        &self,
        run_id: &RunId,
        created_at: DateTime<Utc>,
        run_dir: Option<&str>,
    ) -> Result<Arc<dyn RunStore>>;
    async fn open_run(&self, run_id: &RunId) -> Result<Option<Arc<dyn RunStore>>>;
    async fn open_run_reader(&self, run_id: &RunId) -> Result<Option<Arc<dyn RunStore>>>;
    async fn list_runs(&self, query: &ListRunsQuery) -> Result<Vec<RunSummary>>;
    async fn delete_run(&self, run_id: &RunId) -> Result<()>;
}

#[async_trait]
pub trait RunStore: Send + Sync {
    async fn put_run(&self, record: &RunRecord) -> Result<()>;
    async fn get_run(&self) -> Result<Option<RunRecord>>;

    async fn put_start(&self, record: &StartRecord) -> Result<()>;
    async fn get_start(&self) -> Result<Option<StartRecord>>;

    async fn put_status(&self, record: &RunStatusRecord) -> Result<()>;
    async fn get_status(&self) -> Result<Option<RunStatusRecord>>;

    async fn put_checkpoint(&self, record: &Checkpoint) -> Result<()>;
    async fn get_checkpoint(&self) -> Result<Option<Checkpoint>>;
    async fn append_checkpoint(&self, record: &Checkpoint) -> Result<u32>;
    async fn list_checkpoints(&self) -> Result<Vec<(u32, Checkpoint)>>;

    async fn put_conclusion(&self, record: &Conclusion) -> Result<()>;
    async fn get_conclusion(&self) -> Result<Option<Conclusion>>;

    async fn put_retro(&self, retro: &Retro) -> Result<()>;
    async fn get_retro(&self) -> Result<Option<Retro>>;

    async fn put_graph(&self, dot_source: &str) -> Result<()>;
    async fn get_graph(&self) -> Result<Option<String>>;

    async fn put_sandbox(&self, record: &SandboxRecord) -> Result<()>;
    async fn get_sandbox(&self) -> Result<Option<SandboxRecord>>;

    async fn put_node_prompt(&self, node: &NodeVisitRef<'_>, prompt: &str) -> Result<()>;
    async fn put_node_response(&self, node: &NodeVisitRef<'_>, response: &str) -> Result<()>;
    async fn put_node_status(
        &self,
        node: &NodeVisitRef<'_>,
        status: &NodeStatusRecord,
    ) -> Result<()>;
    async fn put_node_outcome(
        &self,
        node: &NodeVisitRef<'_>,
        outcome: &NodeOutcomeRecord,
    ) -> Result<()>;
    async fn put_node_provider_used(
        &self,
        node: &NodeVisitRef<'_>,
        provider_used: &serde_json::Value,
    ) -> Result<()>;
    async fn put_node_diff(&self, node: &NodeVisitRef<'_>, diff: &str) -> Result<()>;
    async fn put_node_script_invocation(
        &self,
        node: &NodeVisitRef<'_>,
        invocation: &serde_json::Value,
    ) -> Result<()>;
    async fn put_node_script_timing(
        &self,
        node: &NodeVisitRef<'_>,
        timing: &serde_json::Value,
    ) -> Result<()>;
    async fn put_node_parallel_results(
        &self,
        node: &NodeVisitRef<'_>,
        results: &serde_json::Value,
    ) -> Result<()>;
    async fn put_node_stdout(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()>;
    async fn put_node_stderr(&self, node: &NodeVisitRef<'_>, log: &str) -> Result<()>;

    async fn get_node(&self, node: &NodeVisitRef<'_>) -> Result<NodeSnapshot>;
    async fn list_node_visits(&self, node_id: &str) -> Result<Vec<u32>>;
    async fn list_node_ids(&self) -> Result<Vec<String>>;

    async fn put_final_patch(&self, patch: &str) -> Result<()>;
    async fn get_final_patch(&self) -> Result<Option<String>>;

    async fn put_pull_request(&self, record: &PullRequestRecord) -> Result<()>;
    async fn get_pull_request(&self) -> Result<Option<PullRequestRecord>>;

    async fn reset_for_rewind(&self) -> Result<()>;

    async fn append_event(&self, payload: &EventPayload) -> Result<u32>;
    async fn list_events(&self) -> Result<Vec<EventEnvelope>>;
    async fn list_events_from(&self, seq: u32) -> Result<Vec<EventEnvelope>>;
    async fn watch_events_from(
        &self,
        seq: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<EventEnvelope>> + Send>>>;

    async fn put_retro_prompt(&self, text: &str) -> Result<()>;
    async fn get_retro_prompt(&self) -> Result<Option<String>>;
    async fn put_retro_response(&self, text: &str) -> Result<()>;
    async fn get_retro_response(&self) -> Result<Option<String>>;

    async fn put_artifact_value(&self, artifact_id: &str, value: &serde_json::Value) -> Result<()>;
    async fn get_artifact_value(&self, artifact_id: &str) -> Result<Option<serde_json::Value>>;
    async fn list_artifact_values(&self) -> Result<Vec<String>>;

    async fn put_asset(&self, node: &NodeVisitRef<'_>, filename: &str, data: &[u8]) -> Result<()>;
    async fn get_asset(&self, node: &NodeVisitRef<'_>, filename: &str) -> Result<Option<Bytes>>;
    async fn list_assets(&self, node: &NodeVisitRef<'_>) -> Result<Vec<String>>;
    async fn list_all_assets(&self) -> Result<Vec<(String, u32, String)>>;

    async fn get_snapshot(&self) -> Result<Option<RunSnapshot>>;
}
