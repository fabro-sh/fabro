mod auth_codes;
mod auth_tokens;
mod blob_store;
mod projection_cache;
mod run_catalog_index;
mod run_store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use auth_codes::{AuthCode, AuthCodeStore};
pub use auth_tokens::{ConsumeOutcome, RefreshToken, RefreshTokenStore};
pub use blob_store::{Blob, BlobStore};
use chrono::{DateTime, Utc};
use fabro_types::{Run, RunId};
use object_store::ObjectStore;
pub use projection_cache::CachedRunProjection;
use projection_cache::RunProjectionCache;
pub use run_catalog_index::RunCatalogIndex;
pub use run_store::RunDatabase;
use run_store::RunDatabaseInner;
use slatedb::config::{CompressionCodec, Settings};
use tokio::sync::{Mutex, OnceCell};
use tracing::warn;

use crate::{Error, ListRunsQuery, Result, RunProjection, keys};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableRun {
    pub run_id:     RunId,
    pub created_at: DateTime<Utc>,
    pub error:      String,
}

#[derive(Clone)]
pub struct Database {
    object_store: Arc<dyn ObjectStore>,
    base_prefix: String,
    flush_interval: Duration,
    cache_path: Option<PathBuf>,
    db: Arc<OnceCell<slatedb::Db>>,
    active_runs: Arc<Mutex<HashMap<RunId, Arc<RunDatabaseInner>>>>,
    blobs: Arc<OnceCell<Arc<BlobStore>>>,
    catalog_index: Arc<OnceCell<Arc<RunCatalogIndex>>>,
    auth_codes: Arc<OnceCell<Arc<AuthCodeStore>>>,
    refresh_tokens: Arc<OnceCell<Arc<RefreshTokenStore>>>,
    projection_cache: Arc<RunProjectionCache>,
    projection_cache_warmed: Arc<OnceCell<()>>,
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
            projection_cache: Arc::new(RunProjectionCache::default()),
            projection_cache_warmed: Arc::new(OnceCell::new()),
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
        self.warm_projection_cache().await?;
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
        let run_store =
            RunDatabase::open_writer(*run_id, db, Arc::clone(&self.projection_cache)).await?;
        self.cache_active_run(&run_store).await;
        Ok(run_store)
    }

    pub async fn open_run(&self, run_id: &RunId) -> Result<RunDatabase> {
        self.warm_projection_cache().await?;
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
        let run_store =
            RunDatabase::open_writer(*run_id, db, Arc::clone(&self.projection_cache)).await?;
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
        RunDatabase::open_reader(*run_id, db, Arc::clone(&self.projection_cache)).await
    }

    pub async fn list_runs(&self, query: &ListRunsQuery) -> Result<Vec<Run>> {
        Ok(self
            .list_cached_runs(query)
            .await?
            .into_iter()
            .map(|entry| entry.summary)
            .collect())
    }

    pub async fn list_runs_with_projection(
        &self,
        query: &ListRunsQuery,
    ) -> Result<Vec<(Run, RunProjection)>> {
        Ok(self
            .list_cached_runs(query)
            .await?
            .into_iter()
            .map(|entry| (entry.summary, (*entry.projection).clone()))
            .collect())
    }

    pub async fn warm_projection_cache(&self) -> Result<()> {
        self.projection_cache_warmed
            .get_or_try_init(|| async {
                let db = self.open_db().await?;
                let run_ids = self
                    .catalog_index()
                    .await?
                    .list(&ListRunsQuery::default())
                    .await?;
                let mut entries = Vec::new();
                for run_id in run_ids {
                    match RunDatabase::build_cached_projection(&db, &run_id).await {
                        Ok(Some(entry)) => entries.push(entry),
                        Ok(None) => {}
                        Err(err) => {
                            warn!(
                                run_id = %run_id,
                                error = %err,
                                "Skipping run during projection cache warmup"
                            );
                        }
                    }
                }
                self.projection_cache.replace_all(entries).await;
                Ok::<_, Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn list_cached_runs(
        &self,
        query: &ListRunsQuery,
    ) -> Result<Vec<CachedRunProjection>> {
        self.warm_projection_cache().await?;
        Ok(self.projection_cache.list(query).await)
    }

    pub async fn list_unreadable_runs(&self) -> Result<Vec<UnreadableRun>> {
        let db = self.open_db().await?;
        let run_ids = self
            .catalog_index()
            .await?
            .list(&ListRunsQuery::default())
            .await?;
        let mut unreadable = Vec::new();
        for run_id in run_ids {
            match RunDatabase::build_cached_projection(&db, &run_id).await {
                Ok(Some(_)) => {}
                Ok(None) => unreadable.push(UnreadableRun {
                    run_id,
                    created_at: run_id.created_at(),
                    error: "run has no events".to_string(),
                }),
                Err(err) => unreadable.push(UnreadableRun {
                    run_id,
                    created_at: run_id.created_at(),
                    error: err.to_string(),
                }),
            }
        }
        unreadable.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.run_id.cmp(&left.run_id))
        });
        Ok(unreadable)
    }

    pub async fn get_cached_run(&self, run_id: &RunId) -> Result<Option<CachedRunProjection>> {
        self.warm_projection_cache().await?;
        Ok(self.projection_cache.get(run_id).await)
    }

    pub async fn get_cached_summary(&self, run_id: &RunId) -> Result<Option<Run>> {
        self.warm_projection_cache().await?;
        Ok(self.projection_cache.get_summary(run_id).await)
    }

    pub(crate) async fn remove_cached_run(&self, run_id: &RunId) {
        self.projection_cache.remove(run_id).await;
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
        self.remove_cached_run(run_id).await;
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

    pub async fn find(&self, run_id: &RunId) -> Result<Option<Run>> {
        self.db.get_cached_summary(run_id).await
    }

    pub async fn list(&self, query: &ListRunsQuery) -> Result<Vec<Run>> {
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
    use chrono::{DateTime, Utc};
    use fabro_types::{
        AttrValue, FailureReason, Graph, RunControlAction, RunSpec, RunStatus, StageId,
        SuccessReason, WorkflowSettings,
    };
    use futures::TryStreamExt;
    use object_store::memory::InMemory;
    use object_store::path::Path;

    use super::*;
    use crate::{EventPayload, keys};

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
            "run-3" => (
                dt("2026-03-27T12:00:20Z")
                    .timestamp_millis()
                    .cast_unsigned(),
                3,
            ),
            "run-4" => (
                dt("2026-03-27T12:00:30Z")
                    .timestamp_millis()
                    .cast_unsigned(),
                4,
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
            settings: WorkflowSettings::default(),
            graph,
            graph_source: None,
            workflow_slug: Some("night-sky".to_string()),
            source_directory: Some(format!("/tmp/{label}")),
            labels: std::collections::HashMap::from([("team".to_string(), "infra".to_string())]),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
            git: Some(fabro_types::GitContext {
                origin_url:   "https://github.com/fabro-sh/fabro".to_string(),
                branch:       "main".to_string(),
                sha:          None,
                dirty:        fabro_types::DirtyStatus::Clean,
                push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
            }),
            fork_source_ref: None,
        }
    }

    fn event_payload(
        run_id: &str,
        ts: &str,
        event: &str,
        properties: &serde_json::Value,
    ) -> EventPayload {
        event_payload_with_node(run_id, ts, event, properties, None)
    }

    fn event_payload_with_node(
        run_id: &str,
        ts: &str,
        event: &str,
        properties: &serde_json::Value,
        node_id: Option<&str>,
    ) -> EventPayload {
        EventPayload::new(
            serde_json::json!({
                "id": format!("evt-{run_id}-{event}"),
                "ts": ts,
                "run_id": test_run_id(run_id).to_string(),
                "event": event,
                "node_id": node_id,
                "stage_id": node_id.map(|node| format!("{node}@1")),
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
                "source_directory": run_spec.source_directory,
                "run_dir": format!("/tmp/{label}"),
                "git": run_spec.git,
                "labels": run_spec.labels,
            }),
        ))
        .await
        .unwrap();
    }

    async fn append_created_with_parent(
        run: &RunDatabase,
        label: &str,
        created_at: DateTime<Utc>,
        parent_id: RunId,
    ) {
        let run_spec = sample_run_spec(label);
        run.append_event(&event_payload(
            label,
            &created_at.to_rfc3339(),
            "run.created",
            &serde_json::json!({
                "settings": run_spec.settings,
                "graph": run_spec.graph,
                "workflow_slug": run_spec.workflow_slug,
                "source_directory": run_spec.source_directory,
                "run_dir": format!("/tmp/{label}"),
                "git": run_spec.git,
                "labels": run_spec.labels,
                "parent_id": parent_id,
            }),
        ))
        .await
        .unwrap();
    }

    async fn append_completed(run: &RunDatabase, label: &str, created_at: DateTime<Utc>) {
        append_running(run, label, created_at).await;
        run.append_event(&event_payload(
            label,
            "2026-03-27T12:00:03Z",
            "run.completed",
            &serde_json::json!({
                "duration_ms": 3210,
                "artifact_count": 1,
                "status": "succeeded",
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
            "run.starting",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            label,
            "2026-03-27T12:00:02Z",
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
        assert_eq!(summary[0].id, test_run_id("run-2"));
        assert_eq!(summary[1].id, test_run_id("run-1"));
        assert_eq!(summary[1].workflow.name, "night-sky");
        assert_eq!(summary[1].goal, "map the constellations");
        assert_eq!(summary[1].lifecycle.status, RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        });

        let reopened = store.open_run(&test_run_id("run-1")).await.unwrap();
        let stored = reopened.state().await.unwrap().spec;
        assert_eq!(stored.run_id, test_run_id("run-1"));

        store.delete_run(&test_run_id("run-1")).await.unwrap();
        assert!(store.open_run(&test_run_id("run-1")).await.is_err());
        let remaining = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, test_run_id("run-2"));
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
        assert_eq!(summary[0].lifecycle.status, RunStatus::Running);
        assert_eq!(
            summary[0].lifecycle.pending_control,
            Some(RunControlAction::Pause)
        );
    }

    #[tokio::test]
    async fn parent_id_is_projected_from_created_and_parent_events() {
        let (_object_store, store) = make_store();
        let parent_1 = store.create_run(&test_run_id("run-1")).await.unwrap();
        let parent_2 = store.create_run(&test_run_id("run-2")).await.unwrap();
        let child = store.create_run(&test_run_id("run-3")).await.unwrap();
        append_created(&parent_1, "run-1", dt("2026-03-27T12:00:00Z")).await;
        append_created(&parent_2, "run-2", dt("2026-03-27T12:00:10Z")).await;
        append_created_with_parent(
            &child,
            "run-3",
            dt("2026-03-27T12:00:20Z"),
            test_run_id("run-1"),
        )
        .await;

        let initial = store.open_run(&test_run_id("run-3")).await.unwrap();
        assert_eq!(
            initial.state().await.unwrap().parent_id,
            Some(test_run_id("run-1"))
        );
        assert_eq!(
            store
                .get_cached_summary(&test_run_id("run-3"))
                .await
                .unwrap()
                .unwrap()
                .parent_id,
            Some(test_run_id("run-1"))
        );

        child
            .append_event(&event_payload(
                "run-3",
                "2026-03-27T12:00:21Z",
                "run.parent.linked",
                &serde_json::json!({
                    "previous_parent_id": test_run_id("run-1"),
                    "parent_id": test_run_id("run-2"),
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            store
                .get_cached_summary(&test_run_id("run-3"))
                .await
                .unwrap()
                .unwrap()
                .parent_id,
            Some(test_run_id("run-2"))
        );
        assert!(
            store
                .list_runs(&ListRunsQuery {
                    parent_id: Some(test_run_id("run-1")),
                    ..ListRunsQuery::default()
                })
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .list_runs(&ListRunsQuery {
                    parent_id: Some(test_run_id("run-2")),
                    ..ListRunsQuery::default()
                })
                .await
                .unwrap()
                .into_iter()
                .map(|summary| summary.id)
                .collect::<Vec<_>>(),
            vec![test_run_id("run-3")]
        );

        child
            .append_event(&event_payload(
                "run-3",
                "2026-03-27T12:00:22Z",
                "run.parent.unlinked",
                &serde_json::json!({
                    "previous_parent_id": test_run_id("run-2"),
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            store
                .get_cached_summary(&test_run_id("run-3"))
                .await
                .unwrap()
                .unwrap()
                .parent_id,
            None
        );
        assert!(
            store
                .list_runs(&ListRunsQuery {
                    parent_id: Some(test_run_id("run-2")),
                    ..ListRunsQuery::default()
                })
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn list_runs_filters_by_parent_id() {
        let (_object_store, store) = make_store();
        let parent = store.create_run(&test_run_id("run-1")).await.unwrap();
        let child = store.create_run(&test_run_id("run-2")).await.unwrap();
        let unrelated = store.create_run(&test_run_id("run-3")).await.unwrap();
        append_created(&parent, "run-1", dt("2026-03-27T12:00:00Z")).await;
        append_created_with_parent(
            &child,
            "run-2",
            dt("2026-03-27T12:00:10Z"),
            test_run_id("run-1"),
        )
        .await;
        append_created(&unrelated, "run-3", dt("2026-03-27T12:00:20Z")).await;

        let summaries = store
            .list_runs(&ListRunsQuery {
                parent_id: Some(test_run_id("run-1")),
                ..ListRunsQuery::default()
            })
            .await
            .unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, test_run_id("run-2"));
        assert_eq!(summaries[0].parent_id, Some(test_run_id("run-1")));
    }

    #[tokio::test]
    async fn run_summary_includes_children_count() {
        let (_object_store, store) = make_store();
        let parent = store.create_run(&test_run_id("run-1")).await.unwrap();
        let child_a = store.create_run(&test_run_id("run-2")).await.unwrap();
        let child_b = store.create_run(&test_run_id("run-3")).await.unwrap();
        let unrelated = store.create_run(&test_run_id("run-4")).await.unwrap();
        append_created(&parent, "run-1", dt("2026-03-27T12:00:00Z")).await;
        append_created_with_parent(
            &child_a,
            "run-2",
            dt("2026-03-27T12:00:10Z"),
            test_run_id("run-1"),
        )
        .await;
        append_created_with_parent(
            &child_b,
            "run-3",
            dt("2026-03-27T12:00:20Z"),
            test_run_id("run-1"),
        )
        .await;
        append_created(&unrelated, "run-4", dt("2026-03-27T12:00:30Z")).await;

        let summaries = store.list_runs(&ListRunsQuery::default()).await.unwrap();

        let parent_summary = summaries
            .iter()
            .find(|r| r.id == test_run_id("run-1"))
            .expect("parent summary should be present");
        assert_eq!(parent_summary.children_count, 2);

        let child_summary = summaries
            .iter()
            .find(|r| r.id == test_run_id("run-2"))
            .expect("child summary should be present");
        assert_eq!(child_summary.children_count, 0);

        let unrelated_summary = summaries
            .iter()
            .find(|r| r.id == test_run_id("run-4"))
            .expect("unrelated summary should be present");
        assert_eq!(unrelated_summary.children_count, 0);
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
                "failure": {
                    "reason": "cancelled",
                    "detail": {
                        "message": "cancelled",
                        "category": "canceled"
                    }
                },
                "duration_ms": 1,
            }),
        ))
        .await
        .unwrap();

        let summary = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].lifecycle.status, RunStatus::Failed {
            reason: FailureReason::Cancelled,
        });
        assert_eq!(summary[0].lifecycle.pending_control, None);
    }

    #[tokio::test]
    async fn reader_sees_cached_projection_and_recent_events_for_active_run() {
        let (_object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_created(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        let reader = store.open_run_reader(&test_run_id("run-1")).await.unwrap();
        let state = reader.state().await.unwrap();
        assert_eq!(state.spec.run_id, test_run_id("run-1"));

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "run.starting",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:03Z",
            "run.running",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:04Z",
            "run.completed",
            &serde_json::json!({
                "duration_ms": 3210,
                "artifact_count": 1,
                "status": "succeeded",
                "reason": "completed",
                "total_cost": 1.25,
            }),
        ))
        .await
        .unwrap();

        let recent = reader.list_events_from_with_limit(4, 10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].seq, 4);
    }

    #[tokio::test]
    async fn reopening_store_rebuilds_from_shared_db() {
        let (object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_completed(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        let reopened = Database::new(object_store, "runs", Duration::from_millis(1), None);
        let summary = reopened.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].id, test_run_id("run-1"));
        assert_eq!(summary[0].lifecycle.status, RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        });
    }

    #[tokio::test]
    async fn projection_cache_warmup_lists_newest_first_and_applies_date_filters() {
        let (object_store, store) = make_store();
        let run_1 = store.create_run(&test_run_id("run-1")).await.unwrap();
        let run_2 = store.create_run(&test_run_id("run-2")).await.unwrap();
        append_completed(&run_1, "run-1", dt("2026-03-27T12:00:00Z")).await;
        append_running(&run_2, "run-2", dt("2026-03-27T12:00:10Z")).await;

        let reopened = Database::new(object_store, "runs", Duration::from_millis(1), None);
        reopened.warm_projection_cache().await.unwrap();

        let entries = reopened
            .list_cached_runs(&ListRunsQuery::default())
            .await
            .unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.run_id).collect::<Vec<_>>(),
            vec![test_run_id("run-2"), test_run_id("run-1")]
        );
        assert_eq!(entries[0].summary.lifecycle.status, RunStatus::Running);
        assert_eq!(entries[0].projection.spec().run_id, test_run_id("run-2"));
        assert_eq!(entries[0].last_seq, 3);

        let filtered = reopened
            .list_cached_runs(&ListRunsQuery {
                start:     Some(test_run_id("run-2").created_at()),
                end:       Some(test_run_id("run-2").created_at() + chrono::Duration::seconds(1)),
                parent_id: None,
            })
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, test_run_id("run-2"));

        let cached = reopened
            .get_cached_run(&test_run_id("run-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached.summary.lifecycle.status, RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        });
    }

    #[tokio::test]
    async fn projection_cache_warmup_skips_unreplayable_catalog_run() {
        let (object_store, store) = make_store();
        let good_run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_completed(&good_run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        let bad_run_id = test_run_id("run-2");
        store
            .catalog_index()
            .await
            .unwrap()
            .add(&bad_run_id)
            .await
            .unwrap();
        let db = store.open_db().await.unwrap();
        db.put(
            keys::run_event_key(&bad_run_id, 1, 0),
            br#"{"not":"a valid run event"}"#,
        )
        .await
        .unwrap();

        let reopened = Database::new(object_store, "runs", Duration::from_millis(1), None);
        reopened.warm_projection_cache().await.unwrap();

        let entries = reopened
            .list_cached_runs(&ListRunsQuery::default())
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run_id, test_run_id("run-1"));
        assert!(
            reopened
                .get_cached_run(&bad_run_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(reopened.runs().find(&bad_run_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_unreadable_runs_reports_catalog_entries_that_fail_projection() {
        let (object_store, store) = make_store();
        let good_run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_completed(&good_run, "run-1", dt("2026-03-27T12:00:00Z")).await;

        let bad_run_id = test_run_id("run-2");
        store
            .catalog_index()
            .await
            .unwrap()
            .add(&bad_run_id)
            .await
            .unwrap();
        let mut run_spec = serde_json::to_value(sample_run_spec("run-2")).unwrap();
        let run_settings = run_spec
            .get_mut("settings")
            .and_then(|settings| settings.get_mut("run"))
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        run_settings.remove("integrations");
        let db = store.open_db().await.unwrap();
        db.put(
            keys::run_event_key(&bad_run_id, 1, 0),
            serde_json::to_vec(&serde_json::json!({
                "id": "evt-run-2-run.created",
                "ts": "2026-03-27T12:00:10Z",
                "run_id": bad_run_id,
                "event": "run.created",
                "properties": {
                    "settings": run_spec["settings"],
                    "graph": run_spec["graph"],
                    "workflow_slug": run_spec["workflow_slug"],
                    "source_directory": run_spec["source_directory"],
                    "run_dir": "/tmp/run-2",
                    "git": run_spec["git"],
                    "labels": run_spec["labels"],
                },
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let reopened = Database::new(object_store, "runs", Duration::from_millis(1), None);
        let unreadable = reopened.list_unreadable_runs().await.unwrap();

        assert_eq!(unreadable.len(), 1);
        assert_eq!(unreadable[0].run_id, bad_run_id);
        assert_eq!(unreadable[0].created_at, bad_run_id.created_at());
        assert!(
            unreadable[0].error.contains("missing field `integrations`"),
            "expected missing integrations error, got: {}",
            unreadable[0].error
        );
    }

    #[tokio::test]
    async fn append_event_refreshes_projection_cache_and_delete_removes_it() {
        let (_object_store, store) = make_store();
        let run = store.create_run(&test_run_id("run-1")).await.unwrap();
        append_created(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;
        store.warm_projection_cache().await.unwrap();

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:01Z",
            "run.starting",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "run.running",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload_with_node(
            "run-1",
            "2026-03-27T12:00:03Z",
            "stage.started",
            &serde_json::json!({
                "index": 0,
                "handler_type": "prompt",
                "attempt": 1,
                "max_attempts": 1,
            }),
            Some("review"),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload_with_node(
            "run-1",
            "2026-03-27T12:00:04Z",
            "interview.started",
            &serde_json::json!({
                "question_id": "q-1",
                "question": "Approve deploy?",
                "stage": "review",
                "question_type": "yes_no",
                "options": [],
                "allow_freeform": false,
                "context_display": null,
                "timeout_seconds": null,
            }),
            Some("review"),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload_with_node(
            "run-1",
            "2026-03-27T12:00:05Z",
            "checkpoint.completed",
            &serde_json::json!({
                "status": "running",
                "current_node": "review",
                "completed_nodes": [],
                "node_retries": {},
                "context_values": {},
                "node_outcomes": {},
                "next_node_id": "review",
                "git_commit_sha": "abc123",
                "loop_failure_signatures": {},
                "restart_failure_signatures": {},
                "node_visits": { "review": 1 },
            }),
            Some("review"),
        ))
        .await
        .unwrap();

        let cached = store
            .get_cached_run(&test_run_id("run-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached.summary.lifecycle.status, RunStatus::Running);
        assert_eq!(cached.last_seq, 6);
        assert_eq!(
            cached
                .projection
                .stage(&StageId::new("review", 1))
                .unwrap()
                .effective_state(),
            fabro_types::StageState::Running
        );
        assert_eq!(
            cached.projection.pending_interviews["q-1"].question.text,
            "Approve deploy?"
        );
        assert_eq!(
            cached
                .projection
                .current_checkpoint()
                .unwrap()
                .git_commit_sha
                .as_deref(),
            Some("abc123")
        );

        let summaries = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        let projected = store
            .list_runs_with_projection(&ListRunsQuery::default())
            .await
            .unwrap();
        assert_eq!(summaries, vec![cached.summary.clone()]);
        assert_eq!(projected[0].0, cached.summary);
        assert_eq!(
            projected[0]
                .1
                .current_checkpoint()
                .unwrap()
                .git_commit_sha
                .as_deref(),
            cached
                .projection
                .current_checkpoint()
                .unwrap()
                .git_commit_sha
                .as_deref()
        );

        store.delete_run(&test_run_id("run-1")).await.unwrap();
        assert!(
            store
                .get_cached_run(&test_run_id("run-1"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .list_cached_runs(&ListRunsQuery::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn append_event_hydrates_local_projection_cache_for_fresh_writer() {
        let (object_store, store) = make_store();
        let run_id = test_run_id("run-1");
        let run = store.create_run(&run_id).await.unwrap();
        append_created(&run, "run-1", dt("2026-03-27T12:00:00Z")).await;
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:01Z",
            "run.queued",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "run.starting",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:03Z",
            "run.running",
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:04Z",
            "run.failed",
            &serde_json::json!({
                "failure": {
                    "reason": "workflow_error",
                    "detail": {
                        "message": "workflow failed",
                        "category": "deterministic"
                    }
                },
                "duration_ms": 1,
            }),
        ))
        .await
        .unwrap();

        let reopened = Database::new(
            Arc::clone(&object_store),
            "runs/",
            Duration::from_millis(1),
            None,
        );
        let fresh_writer = reopened.open_run(&run_id).await.unwrap();
        fresh_writer
            .append_event(&event_payload(
                "run-1",
                "2026-03-27T12:00:05Z",
                "run.title.updated",
                &serde_json::json!({ "title": "Renamed failed run" }),
            ))
            .await
            .unwrap();

        let state = fresh_writer.state().await.unwrap();
        assert_eq!(state.title, "Renamed failed run");
        assert_eq!(state.status, RunStatus::Failed {
            reason: FailureReason::WorkflowError,
        });

        let cached = reopened.get_cached_run(&run_id).await.unwrap().unwrap();
        assert_eq!(cached.summary.title, "Renamed failed run");
        assert_eq!(cached.summary.lifecycle.status, RunStatus::Failed {
            reason: FailureReason::WorkflowError,
        });
    }
}
