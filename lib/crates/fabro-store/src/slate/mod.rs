mod catalog;
mod run_store;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use object_store::ObjectStore;
use slatedb::config::Settings;
use tokio::sync::{Mutex, OnceCell};

use crate::keys;
use crate::{ListRunsQuery, Result, RunSummary, StoreError};
use fabro_types::RunId;
use run_store::SlateRunStoreInner;
pub use run_store::{NodeArtifact, SlateRunStore};

#[derive(Clone)]
pub struct SlateStore {
    object_store: Arc<dyn ObjectStore>,
    base_prefix: String,
    flush_interval: Duration,
    db: Arc<OnceCell<slatedb::Db>>,
    active_runs: Arc<Mutex<HashMap<RunId, Arc<SlateRunStoreInner>>>>,
}

impl std::fmt::Debug for SlateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlateStore")
            .field("base_prefix", &self.base_prefix)
            .field("flush_interval", &self.flush_interval)
            .finish_non_exhaustive()
    }
}

impl SlateStore {
    pub fn new(
        object_store: Arc<dyn ObjectStore>,
        base_prefix: impl Into<String>,
        flush_interval: Duration,
    ) -> Self {
        Self {
            object_store,
            base_prefix: normalize_base_prefix(base_prefix.into()),
            flush_interval,
            db: Arc::new(OnceCell::new()),
            active_runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn shared_db_prefix(&self) -> String {
        format!("{}db", self.base_prefix)
    }

    async fn open_db(&self) -> Result<slatedb::Db> {
        let db = self
            .db
            .get_or_try_init(|| async {
                slatedb::Db::builder(self.shared_db_prefix(), self.object_store.clone())
                    .with_settings(Settings {
                        flush_interval: Some(self.flush_interval),
                        ..Settings::default()
                    })
                    .build()
                    .await
            })
            .await?;
        Ok(db.clone())
    }

    async fn get_active_run(&self, run_id: &RunId) -> Option<SlateRunStore> {
        let active_runs = self.active_runs.lock().await;
        active_runs
            .get(run_id)
            .cloned()
            .map(SlateRunStore::from_inner)
    }

    async fn cache_active_run(&self, run_store: &SlateRunStore) {
        self.active_runs
            .lock()
            .await
            .insert(run_store.run_id(), run_store.inner_arc());
    }

    async fn remove_active_run(&self, run_id: &RunId) -> Option<SlateRunStore> {
        self.active_runs
            .lock()
            .await
            .remove(run_id)
            .map(SlateRunStore::from_inner)
    }

    pub async fn create_run(&self, run_id: &RunId) -> Result<SlateRunStore> {
        let db = self.open_db().await?;
        let locator_exists = catalog::read_locator(&db, run_id).await?;

        if let Some(active) = self.get_active_run(run_id).await {
            if locator_exists && !active.matches_run(run_id) {
                return Err(StoreError::RunAlreadyExists(run_id.to_string()));
            }
            catalog::write_catalog(&db, run_id).await?;
            return Ok(active);
        }

        if locator_exists {
            return Err(StoreError::RunAlreadyExists(run_id.to_string()));
        }

        SlateRunStore::validate_init(&db, run_id).await?;
        db.put(keys::init_key(run_id), serde_json::to_vec(run_id)?)
            .await?;
        catalog::write_catalog(&db, run_id).await?;
        let run_store = SlateRunStore::open_writer(*run_id, db).await?;
        self.cache_active_run(&run_store).await;
        Ok(run_store)
    }

    pub async fn open_run(&self, run_id: &RunId) -> Result<SlateRunStore> {
        let db = self.open_db().await?;
        if let Some(active) = self.get_active_run(run_id).await {
            if !active.matches_run(run_id) {
                return Err(StoreError::Other(format!(
                    "active run cache mismatch for run_id {run_id:?}"
                )));
            }
            return Ok(active);
        }
        if !catalog::read_locator(&db, run_id).await? {
            return Err(StoreError::RunNotFound(run_id.to_string()));
        }
        if !SlateRunStore::validate_init(&db, run_id).await? {
            return Err(StoreError::RunNotFound(run_id.to_string()));
        }
        let run_store = SlateRunStore::open_writer(*run_id, db).await?;
        self.cache_active_run(&run_store).await;
        Ok(run_store)
    }

    pub async fn open_run_reader(&self, run_id: &RunId) -> Result<SlateRunStore> {
        let db = self.open_db().await?;
        if let Some(active) = self.get_active_run(run_id).await {
            if !active.matches_run(run_id) {
                return Err(StoreError::Other(format!(
                    "active run cache mismatch for run_id {run_id:?}"
                )));
            }
            return Ok(active.read_only_clone());
        }
        if !catalog::read_locator(&db, run_id).await? {
            return Err(StoreError::RunNotFound(run_id.to_string()));
        }
        if !SlateRunStore::validate_init(&db, run_id).await? {
            return Err(StoreError::RunNotFound(run_id.to_string()));
        }
        SlateRunStore::open_reader(*run_id, db).await
    }

    pub async fn list_runs(&self, query: &ListRunsQuery) -> Result<Vec<RunSummary>> {
        let db = self.open_db().await?;
        let run_ids = catalog::list_run_ids(&db, query).await?;
        let mut summaries = Vec::new();
        for run_id in run_ids {
            if let Some(active) = self.get_active_run(&run_id).await {
                summaries.push(active.state().await?.build_summary(&run_id));
                continue;
            }
            if !SlateRunStore::validate_init(&db, &run_id).await? {
                continue;
            }
            summaries.push(SlateRunStore::build_summary(&db, &run_id).await?);
        }
        summaries.sort_by(|a, b| b.run_id.created_at().cmp(&a.run_id.created_at()));
        Ok(summaries)
    }

    pub async fn delete_run(&self, run_id: &RunId) -> Result<()> {
        let active = self.remove_active_run(run_id).await;
        if let Some(active) = &active {
            active.close().await?;
        }

        let db = self.open_db().await?;
        let prefix = keys::run_prefix(run_id);
        let mut iter = db.scan_prefix(prefix.as_bytes()).await?;
        let mut keys_to_delete = Vec::new();
        while let Some(entry) = iter.next().await? {
            keys_to_delete.push(String::from_utf8(entry.key.to_vec()).map_err(|err| {
                StoreError::Other(format!("stored key is not valid UTF-8: {err}"))
            })?);
        }
        for key in keys_to_delete {
            db.delete(key).await?;
        }
        catalog::delete_catalog(&db, run_id).await?;
        Ok(())
    }
}

pub(crate) fn normalize_base_prefix(prefix: String) -> String {
    if prefix.is_empty() {
        return String::new();
    }
    if prefix.ends_with('/') {
        prefix
    } else {
        format!("{prefix}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{DateTime, Utc};
    use fabro_types::{AttrValue, Graph, RunRecord, RunStatus, Settings, StatusReason};
    use futures::TryStreamExt;
    use object_store::memory::InMemory;
    use object_store::path::Path;
    use std::path::PathBuf;

    use crate::EventPayload;

    fn dt(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    fn test_run_id(label: &str) -> RunId {
        let (timestamp_ms, random) = match label {
            "run-1" => (
                dt("2026-03-27T12:00:00Z")
                    .timestamp_millis()
                    .cast_unsigned(),
                1,
            ),
            "run-2" => (
                dt("2026-03-27T12:00:10Z")
                    .timestamp_millis()
                    .cast_unsigned(),
                2,
            ),
            _ => panic!("unknown test run id: {label}"),
        };
        RunId::from(ulid::Ulid::from_parts(timestamp_ms, random))
    }

    fn make_store() -> (Arc<dyn ObjectStore>, SlateStore) {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = SlateStore::new(object_store.clone(), "runs/", Duration::from_millis(1));
        (object_store, store)
    }

    fn sample_run_record(label: &str) -> RunRecord {
        let mut graph = Graph::new("night-sky");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("map the constellations".to_string()),
        );
        RunRecord {
            run_id: test_run_id(label),
            settings: Settings::default(),
            graph,
            workflow_slug: Some("night-sky".to_string()),
            working_directory: PathBuf::from(format!("/tmp/{label}")),
            host_repo_path: Some("github.com/fabro-sh/fabro".to_string()),
            base_branch: Some("main".to_string()),
            labels: std::collections::HashMap::from([("team".to_string(), "infra".to_string())]),
        }
    }

    fn event_payload(
        run_id: &str,
        ts: &str,
        event: &str,
        properties: &serde_json::Value,
    ) -> EventPayload {
        EventPayload::new(
            serde_json::json!({
                "id": format!("evt-{run_id}-{event}"),
                "ts": ts,
                "run_id": test_run_id(run_id).to_string(),
                "event": event,
                "properties": properties,
            }),
            &test_run_id(run_id),
        )
        .unwrap()
    }

    async fn append_created(run: &SlateRunStore, label: &str, created_at: DateTime<Utc>) {
        let run_record = sample_run_record(label);
        run.append_event(&event_payload(
            label,
            &created_at.to_rfc3339(),
            "run.created",
            &serde_json::json!({
                "settings": run_record.settings,
                "graph": run_record.graph,
                "workflow_slug": run_record.workflow_slug,
                "working_directory": run_record.working_directory,
                "run_dir": format!("/tmp/{label}"),
                "host_repo_path": run_record.host_repo_path,
                "base_branch": run_record.base_branch,
                "labels": run_record.labels,
            }),
        ))
        .await
        .unwrap();
    }

    async fn append_completed(run: &SlateRunStore, label: &str, created_at: DateTime<Utc>) {
        append_created(run, label, created_at).await;
        run.append_event(&event_payload(
            label,
            "2026-03-27T12:00:02Z",
            "run.completed",
            &serde_json::json!({
                "duration_ms": 3210,
                "artifact_count": 1,
                "status": "success",
                "reason": "completed",
                "total_cost": 1.25,
            }),
        ))
        .await
        .unwrap();
    }

    async fn list_paths(store: Arc<dyn ObjectStore>, prefix: &str) -> Vec<String> {
        let mut items = store
            .list(Some(&Path::from(prefix.to_string())))
            .map_ok(|meta| meta.location.to_string())
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        items.sort();
        items
    }

    #[tokio::test]
    async fn create_open_list_and_delete_full_lifecycle_in_shared_db() {
        let (object_store, store) = make_store();
        let run_1 = store.create_run(&test_run_id("run-1")).await.unwrap();
        let run_2 = store.create_run(&test_run_id("run-2")).await.unwrap();
        append_completed(&run_1, "run-1", dt("2026-03-27T12:00:00Z")).await;
        append_created(&run_2, "run-2", dt("2026-03-27T12:00:10Z")).await;

        let summary = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].run_id, test_run_id("run-2"));
        assert_eq!(summary[1].run_id, test_run_id("run-1"));
        assert_eq!(summary[1].workflow_name, Some("night-sky".to_string()));
        assert_eq!(summary[1].goal, Some("map the constellations".to_string()));
        assert_eq!(summary[1].status, Some(RunStatus::Succeeded));
        assert_eq!(summary[1].status_reason, Some(StatusReason::Completed));

        let reopened = store.open_run(&test_run_id("run-1")).await.unwrap();
        let stored = reopened.state().await.unwrap().run.unwrap();
        assert_eq!(stored.run_id, test_run_id("run-1"));

        store.delete_run(&test_run_id("run-1")).await.unwrap();
        assert!(store.open_run(&test_run_id("run-1")).await.is_err());
        let remaining = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].run_id, test_run_id("run-2"));
        assert!(!list_paths(object_store, "runs/db").await.is_empty());
    }

    #[tokio::test]
    async fn open_run_reader_is_read_only() {
        let (_object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_created(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        let reader = store.open_run_reader(&test_run_id("run-1")).await.unwrap();
        let err = reader
            .append_event(&event_payload(
                "run-1",
                "2026-03-27T12:00:01Z",
                "run.completed",
                &serde_json::json!({ "reason": "completed" }),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::ReadOnly));
    }

    #[tokio::test]
    async fn reader_sees_cached_projection_and_recent_events_for_active_run() {
        let (_object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_created(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        let reader = store.open_run_reader(&test_run_id("run-1")).await.unwrap();
        let state = reader.state().await.unwrap();
        assert_eq!(state.run.unwrap().run_id, test_run_id("run-1"));

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "run.completed",
            &serde_json::json!({
                "duration_ms": 3210,
                "artifact_count": 1,
                "status": "success",
                "reason": "completed",
                "total_cost": 1.25,
            }),
        ))
        .await
        .unwrap();

        let recent = reader.list_events_from_with_limit(2, 10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].seq, 2);
    }

    #[tokio::test]
    async fn reopening_store_rebuilds_from_shared_db() {
        let (object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_completed(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        let reopened = SlateStore::new(object_store, "runs", Duration::from_millis(1));
        let summary = reopened.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].run_id, test_run_id("run-1"));
        assert_eq!(summary[0].status, Some(RunStatus::Succeeded));
    }
}
