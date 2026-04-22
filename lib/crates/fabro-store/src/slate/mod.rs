mod auth_codes;
mod auth_tokens;
mod blob_store;
mod run_catalog_index;
mod run_store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use auth_codes::{AuthCode, AuthCodeStore};
pub use auth_tokens::{ConsumeOutcome, RefreshToken, RefreshTokenStore};
pub use blob_store::{Blob, BlobStore};
use fabro_types::{RunId, RunSummary};
use object_store::ObjectStore;
pub use run_catalog_index::RunCatalogIndex;
pub use run_store::RunDatabase;
use run_store::RunDatabaseInner;
use slatedb::config::{CompressionCodec, Settings};
use tokio::sync::{Mutex, OnceCell};

use crate::run_state::build_summary;
use crate::{Error, ListRunsQuery, Result, keys};

#[derive(Clone)]
pub struct Database {
    object_store:   Arc<dyn ObjectStore>,
    base_prefix:    String,
    flush_interval: Duration,
    cache_path:     Option<PathBuf>,
    db:             Arc<OnceCell<slatedb::Db>>,
    active_runs:    Arc<Mutex<HashMap<RunId, Arc<RunDatabaseInner>>>>,
    blobs:          Arc<OnceCell<Arc<BlobStore>>>,
    catalog_index:  Arc<OnceCell<Arc<RunCatalogIndex>>>,
    auth_codes:     Arc<OnceCell<Arc<AuthCodeStore>>>,
    refresh_tokens: Arc<OnceCell<Arc<RefreshTokenStore>>>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("base_prefix", &self.base_prefix)
            .field("flush_interval", &self.flush_interval)
            .field("cache_path", &self.cache_path)
            .finish_non_exhaustive()
    }
}

impl Database {
    pub fn new(
        object_store: Arc<dyn ObjectStore>,
        base_prefix: impl Into<String>,
        flush_interval: Duration,
        cache_path: Option<PathBuf>,
    ) -> Self {
        Self {
            object_store,
            base_prefix: normalize_base_prefix(base_prefix.into()),
            flush_interval,
            cache_path,
            db: Arc::new(OnceCell::new()),
            active_runs: Arc::new(Mutex::new(HashMap::new())),
            blobs: Arc::new(OnceCell::new()),
            catalog_index: Arc::new(OnceCell::new()),
            auth_codes: Arc::new(OnceCell::new()),
            refresh_tokens: Arc::new(OnceCell::new()),
        }
    }

    fn shared_db_prefix(&self) -> String {
        self.base_prefix.clone()
    }

    async fn open_db(&self) -> Result<slatedb::Db> {
        let db = self
            .db
            .get_or_try_init(|| async {
                let mut settings = Settings {
                    flush_interval: Some(self.flush_interval),
                    compression_codec: Some(CompressionCodec::Zstd),
                    ..Settings::default()
                };
                if let Some(ref cache_path) = self.cache_path {
                    settings.object_store_cache_options.root_folder = Some(cache_path.clone());
                }
                slatedb::Db::builder(self.shared_db_prefix(), self.object_store.clone())
                    .with_settings(settings)
                    .build()
                    .await
            })
            .await?;
        Ok(db.clone())
    }

    async fn get_active_run(&self, run_id: &RunId) -> Option<RunDatabase> {
        let active_runs = self.active_runs.lock().await;
        active_runs
            .get(run_id)
            .cloned()
            .map(RunDatabase::from_inner)
    }

    async fn cache_active_run(&self, run_store: &RunDatabase) {
        self.active_runs
            .lock()
            .await
            .insert(run_store.run_id(), run_store.inner_arc());
    }

    async fn remove_active_run(&self, run_id: &RunId) -> Option<RunDatabase> {
        self.active_runs
            .lock()
            .await
            .remove(run_id)
            .map(RunDatabase::from_inner)
    }

    pub async fn create_run(&self, run_id: &RunId) -> Result<RunDatabase> {
        let db = self.open_db().await?;
        let run_exists = RunDatabase::has_any_events(&db, run_id).await?;

        if let Some(active) = self.get_active_run(run_id).await {
            if run_exists && !active.matches_run(run_id) {
                return Err(Error::RunAlreadyExists(run_id.to_string()));
            }
            self.catalog_index().await?.add(run_id).await?;
            return Ok(active);
        }

        if run_exists {
            return Err(Error::RunAlreadyExists(run_id.to_string()));
        }

        self.catalog_index().await?.add(run_id).await?;
        let run_store = RunDatabase::open_writer(*run_id, db).await?;
        self.cache_active_run(&run_store).await;
        Ok(run_store)
    }

    pub async fn open_run(&self, run_id: &RunId) -> Result<RunDatabase> {
        let db = self.open_db().await?;
        if let Some(active) = self.get_active_run(run_id).await {
            if !active.matches_run(run_id) {
                return Err(Error::Other(format!(
                    "active run cache mismatch for run_id {run_id:?}"
                )));
            }
            return Ok(active);
        }
        if !RunDatabase::has_any_events(&db, run_id).await? {
            return Err(Error::RunNotFound(run_id.to_string()));
        }
        let run_store = RunDatabase::open_writer(*run_id, db).await?;
        self.cache_active_run(&run_store).await;
        Ok(run_store)
    }

    pub async fn open_run_reader(&self, run_id: &RunId) -> Result<RunDatabase> {
        let db = self.open_db().await?;
        if let Some(active) = self.get_active_run(run_id).await {
            if !active.matches_run(run_id) {
                return Err(Error::Other(format!(
                    "active run cache mismatch for run_id {run_id:?}"
                )));
            }
            return Ok(active.read_only_clone());
        }
        if !RunDatabase::has_any_events(&db, run_id).await? {
            return Err(Error::RunNotFound(run_id.to_string()));
        }
        RunDatabase::open_reader(*run_id, db).await
    }

    pub async fn list_runs(&self, query: &ListRunsQuery) -> Result<Vec<RunSummary>> {
        let db = self.open_db().await?;
        let run_ids = self.catalog_index().await?.list(query).await?;
        let mut summaries = Vec::new();
        for run_id in run_ids {
            if let Some(active) = self.get_active_run(&run_id).await {
                summaries.push(build_summary(&active.state().await?, &run_id));
                continue;
            }
            if !RunDatabase::has_any_events(&db, &run_id).await? {
                continue;
            }
            summaries.push(RunDatabase::build_summary(&db, &run_id).await?);
        }
        summaries.sort_by_key(|b| std::cmp::Reverse(b.run_id.created_at()));
        Ok(summaries)
    }

    pub async fn delete_run(&self, run_id: &RunId) -> Result<()> {
        let active = self.remove_active_run(run_id).await;
        if let Some(active) = &active {
            active.close().await?;
        }

        let db = self.open_db().await?;
        let mut keys_to_delete = Vec::new();
        for prefix in [keys::run_data_prefix(run_id)] {
            let mut iter = db.scan_prefix(&prefix).await?;
            while let Some(entry) = iter.next().await? {
                keys_to_delete.push(String::from_utf8(entry.key.to_vec()).map_err(|err| {
                    Error::Other(format!("stored key is not valid UTF-8: {err}"))
                })?);
            }
        }
        for key in keys_to_delete {
            db.delete(key).await?;
        }
        self.catalog_index().await?.remove(run_id).await?;
        Ok(())
    }

    pub async fn auth_codes(&self) -> Result<Arc<AuthCodeStore>> {
        let store = self
            .auth_codes
            .get_or_try_init(|| async {
                let db = Arc::new(self.open_db().await?);
                Ok::<_, Error>(Arc::new(AuthCodeStore::new(db)))
            })
            .await?;
        Ok(Arc::clone(store))
    }

    pub async fn catalog_index(&self) -> Result<Arc<RunCatalogIndex>> {
        let store = self
            .catalog_index
            .get_or_try_init(|| async {
                let db = Arc::new(self.open_db().await?);
                Ok::<_, Error>(Arc::new(RunCatalogIndex::new(db)))
            })
            .await?;
        Ok(Arc::clone(store))
    }

    pub async fn blobs(&self) -> Result<Arc<BlobStore>> {
        let store = self
            .blobs
            .get_or_try_init(|| async {
                let db = Arc::new(self.open_db().await?);
                Ok::<_, Error>(Arc::new(BlobStore::new(db)))
            })
            .await?;
        Ok(Arc::clone(store))
    }

    pub async fn refresh_tokens(&self) -> Result<Arc<RefreshTokenStore>> {
        let store = self
            .refresh_tokens
            .get_or_try_init(|| async {
                let db = Arc::new(self.open_db().await?);
                Ok::<_, Error>(Arc::new(RefreshTokenStore::new(db)))
            })
            .await?;
        Ok(Arc::clone(store))
    }

    #[must_use]
    pub fn runs(&self) -> Runs {
        Runs { db: self.clone() }
    }
}

#[derive(Clone, Debug)]
pub struct Runs {
    db: Database,
}

impl Runs {
    pub async fn get(&self, run_id: &RunId) -> Result<RunDatabase> {
        self.db.open_run(run_id).await
    }

    pub async fn find(&self, run_id: &RunId) -> Result<Option<RunSummary>> {
        match self.db.open_run_reader(run_id).await {
            Ok(run_db) => Ok(Some(build_summary(&run_db.state().await?, run_id))),
            Err(Error::RunNotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub async fn list(&self, query: &ListRunsQuery) -> Result<Vec<RunSummary>> {
        self.db.list_runs(query).await
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
    use std::path::PathBuf;

    use chrono::{DateTime, Utc};
    use fabro_types::settings::SettingsLayer;
    use fabro_types::{
        AttrValue, FailureReason, Graph, RunControlAction, RunSpec, RunStatus, SuccessReason,
    };
    use futures::TryStreamExt;
    use object_store::memory::InMemory;
    use object_store::path::Path;

    use super::*;
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

    fn make_store() -> (Arc<dyn ObjectStore>, Database) {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = Database::new(
            object_store.clone(),
            "runs/",
            Duration::from_millis(1),
            None,
        );
        (object_store, store)
    }

    fn sample_run_spec(label: &str) -> RunSpec {
        let mut graph = Graph::new("night-sky");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("map the constellations".to_string()),
        );
        RunSpec {
            run_id: test_run_id(label),
            settings: SettingsLayer::default(),
            graph,
            workflow_slug: Some("night-sky".to_string()),
            working_directory: PathBuf::from(format!("/tmp/{label}")),
            host_repo_path: Some("github.com/fabro-sh/fabro".to_string()),
            repo_origin_url: Some("https://github.com/fabro-sh/fabro".to_string()),
            base_branch: Some("main".to_string()),
            labels: std::collections::HashMap::from([("team".to_string(), "infra".to_string())]),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
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

    async fn append_created(run: &RunDatabase, label: &str, created_at: DateTime<Utc>) {
        let run_spec = sample_run_spec(label);
        run.append_event(&event_payload(
            label,
            &created_at.to_rfc3339(),
            "run.created",
            &serde_json::json!({
                "settings": run_spec.settings,
                "graph": run_spec.graph,
                "workflow_slug": run_spec.workflow_slug,
                "working_directory": run_spec.working_directory,
                "run_dir": format!("/tmp/{label}"),
                "host_repo_path": run_spec.host_repo_path,
                "base_branch": run_spec.base_branch,
                "labels": run_spec.labels,
            }),
        ))
        .await
        .unwrap();
    }

    async fn append_completed(run: &RunDatabase, label: &str, created_at: DateTime<Utc>) {
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

    async fn append_running(run: &RunDatabase, label: &str, created_at: DateTime<Utc>) {
        append_created(run, label, created_at).await;
        run.append_event(&event_payload(
            label,
            "2026-03-27T12:00:01Z",
            "run.running",
            &serde_json::json!({}),
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
        assert_eq!(
            summary[1].status,
            RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            }
        );

        let reopened = store.open_run(&test_run_id("run-1")).await.unwrap();
        let stored = reopened.state().await.unwrap().spec.unwrap();
        assert_eq!(stored.run_id, test_run_id("run-1"));

        store.delete_run(&test_run_id("run-1")).await.unwrap();
        assert!(store.open_run(&test_run_id("run-1")).await.is_err());
        let remaining = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].run_id, test_run_id("run-2"));
        assert!(!list_paths(object_store, "runs/").await.is_empty());
    }

    #[tokio::test]
    async fn delete_run_keeps_global_cas_blobs() {
        let (_object_store, store) = make_store();
        let run_1 = store.create_run(&test_run_id("run-1")).await.unwrap();
        let run_2 = store.create_run(&test_run_id("run-2")).await.unwrap();
        append_created(&run_1, "run-1", dt("2026-03-27T12:00:00Z")).await;
        append_created(&run_2, "run-2", dt("2026-03-27T12:00:10Z")).await;

        let shared_blob = br#"{"summary":"shared"}"#;
        let shared_blob_id = run_1.write_blob(shared_blob).await.unwrap();

        store.delete_run(&test_run_id("run-1")).await.unwrap();

        let reopened = store.open_run(&test_run_id("run-2")).await.unwrap();
        let read = reopened.read_blob(&shared_blob_id).await.unwrap();
        assert_eq!(read.as_deref(), Some(shared_blob.as_slice()));
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
        assert!(matches!(err, Error::ReadOnly));
    }

    #[tokio::test]
    async fn control_request_events_set_pending_control_without_overwriting_status() {
        let (_object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_running(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "run.pause.requested",
            &serde_json::json!({ "action": "pause" }),
        ))
        .await
        .unwrap();

        let summary = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].status, RunStatus::Running);
        assert_eq!(summary[0].pending_control, Some(RunControlAction::Pause));
    }

    #[tokio::test]
    async fn control_effect_events_clear_pending_control_and_update_status() {
        let (_object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_running(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "run.pause.requested",
            &serde_json::json!({ "action": "pause" }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:03Z",
            "run.paused",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:04Z",
            "run.unpause.requested",
            &serde_json::json!({ "action": "unpause" }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:05Z",
            "run.unpaused",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:06Z",
            "run.cancel.requested",
            &serde_json::json!({ "action": "cancel" }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:07Z",
            "run.failed",
            &serde_json::json!({
                "error": "cancelled",
                "duration_ms": 1,
                "reason": "cancelled",
            }),
        ))
        .await
        .unwrap();

        let summary = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(
            summary[0].status,
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            }
        );
        assert_eq!(summary[0].pending_control, None);
    }

    #[tokio::test]
    async fn reader_sees_cached_projection_and_recent_events_for_active_run() {
        let (_object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_created(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        let reader = store.open_run_reader(&test_run_id("run-1")).await.unwrap();
        let state = reader.state().await.unwrap();
        assert_eq!(state.spec.unwrap().run_id, test_run_id("run-1"));

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

        let reopened = Database::new(object_store, "runs", Duration::from_millis(1), None);
        let summary = reopened.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].run_id, test_run_id("run-1"));
        assert_eq!(
            summary[0].status,
            RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            }
        );
    }
}
