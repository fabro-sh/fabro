use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use fabro_store::{EventEnvelope, RunDatabase, RunProjection};
use fabro_types::{RunBlobId, RunEvent};

use crate::event::build_redacted_event_payload;

#[async_trait]
pub trait RunStoreBackend: Send + Sync {
    async fn load_state(&self) -> Result<RunProjection>;
    async fn list_events(&self) -> Result<Vec<EventEnvelope>>;
    async fn append_run_event(&self, event: &RunEvent) -> Result<()>;
    async fn write_blob(&self, data: &[u8]) -> Result<RunBlobId>;
    async fn read_blob(&self, id: &RunBlobId) -> Result<Option<Bytes>>;
    async fn read_run_log(&self) -> Result<Option<Vec<u8>>>;
}

#[derive(Clone)]
pub struct RunStoreHandle {
    backend: Arc<dyn RunStoreBackend>,
}

impl RunStoreHandle {
    #[must_use]
    pub fn new(backend: Arc<dyn RunStoreBackend>) -> Self {
        Self { backend }
    }

    #[must_use]
    pub fn local(run_store: RunDatabase) -> Self {
        Self::new(Arc::new(LocalRunStoreBackend { run_store }))
    }

    pub async fn state(&self) -> Result<RunProjection> {
        self.backend.load_state().await
    }

    pub async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        self.backend.list_events().await
    }

    pub async fn append_run_event(&self, event: &RunEvent) -> Result<()> {
        self.backend.append_run_event(event).await
    }

    pub async fn write_blob(&self, data: &[u8]) -> Result<RunBlobId> {
        self.backend.write_blob(data).await
    }

    pub async fn read_blob(&self, id: &RunBlobId) -> Result<Option<Bytes>> {
        self.backend.read_blob(id).await
    }

    pub async fn read_run_log(&self) -> Result<Option<Vec<u8>>> {
        self.backend.read_run_log().await
    }
}

impl From<RunDatabase> for RunStoreHandle {
    fn from(value: RunDatabase) -> Self {
        Self::local(value)
    }
}

struct LocalRunStoreBackend {
    run_store: RunDatabase,
}

#[async_trait]
impl RunStoreBackend for LocalRunStoreBackend {
    async fn load_state(&self) -> Result<RunProjection> {
        self.run_store.state().await.map_err(anyhow::Error::from)
    }

    async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
        self.run_store
            .list_events()
            .await
            .map_err(anyhow::Error::from)
    }

    async fn append_run_event(&self, event: &RunEvent) -> Result<()> {
        let payload = build_redacted_event_payload(event, &event.run_id)?;
        self.run_store
            .append_event(&payload)
            .await
            .map(|_| ())
            .map_err(anyhow::Error::from)
    }

    async fn write_blob(&self, data: &[u8]) -> Result<RunBlobId> {
        self.run_store
            .write_blob(data)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn read_blob(&self, id: &RunBlobId) -> Result<Option<Bytes>> {
        self.run_store
            .read_blob(id)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn read_run_log(&self) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::Utc;
    use fabro_graphviz::graph::Graph;
    use fabro_store::Database;
    use fabro_types::run_event::RunSubmittedProps;
    use fabro_types::{EventBody, RunEvent, WorkflowSettings, fixtures};
    use object_store::memory::InMemory;

    use super::RunStoreHandle;
    use crate::event::{Event, append_event};
    use crate::records::RunSpec;

    async fn test_run_store() -> fabro_store::RunDatabase {
        let store = Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ));
        store.create_run(&fixtures::RUN_1).await.unwrap()
    }

    fn test_run_spec() -> RunSpec {
        RunSpec {
            run_id:           fixtures::RUN_1,
            settings:         WorkflowSettings::default(),
            graph:            Graph::new("test"),
            workflow_slug:    Some("test".to_string()),
            source_directory: Some("/tmp/test".to_string()),
            git:              None,
            labels:           HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        }
    }

    #[tokio::test]
    async fn local_handle_loads_state_and_events() {
        let run_store = test_run_store().await;
        let record = test_run_spec();
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:           fixtures::RUN_1,
            settings:         serde_json::to_value(&record.settings).unwrap(),
            graph:            serde_json::to_value(&record.graph).unwrap(),
            workflow_source:  Some("digraph test {}".to_string()),
            workflow_config:  None,
            labels:           std::collections::BTreeMap::new(),
            run_dir:          "/tmp/test".to_string(),
            source_directory: Some("/tmp/test".to_string()),
            workflow_slug:    Some("test".to_string()),
            db_prefix:        None,
            provenance:       None,
            manifest_blob:    None,
            git:              None,
            fork_source_ref:  None,
            in_place:         false,
            web_url:          None,
        })
        .await
        .unwrap();

        let handle = RunStoreHandle::local(run_store);
        let state = handle.state().await.unwrap();
        let events = handle.list_events().await.unwrap();

        assert_eq!(state.spec.unwrap().workflow_slug.as_deref(), Some("test"));
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn local_handle_appends_events_and_roundtrips_blobs() {
        let run_store = test_run_store().await;
        let handle = RunStoreHandle::local(run_store);

        let event = RunEvent {
            id:                 "evt-run-submitted".to_string(),
            ts:                 Utc::now(),
            run_id:             fixtures::RUN_1,
            node_id:            None,
            node_label:         None,
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::RunSubmitted(RunSubmittedProps {
                definition_blob: None,
            }),
        };
        handle.append_run_event(&event).await.unwrap();

        let blob_id = handle.write_blob(br#"{"ok":true}"#).await.unwrap();
        let blob = handle.read_blob(&blob_id).await.unwrap().unwrap();
        let events = handle.list_events().await.unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(blob.as_ref(), br#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn local_handle_returns_no_run_log() {
        let run_store = test_run_store().await;
        let handle = RunStoreHandle::local(run_store);

        assert_eq!(handle.read_run_log().await.unwrap(), None);
    }
}
