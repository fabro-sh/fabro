mod catalog;
mod run_store;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use object_store::ObjectStore;
use object_store::path::Path;
use slatedb::DbReader;
use slatedb::config::{DbReaderOptions, Settings};
use tokio::sync::Mutex;

use crate::keys;
use crate::{CatalogRecord, ListRunsQuery, Result, RunStore, RunSummary, Store, StoreError};
use fabro_types::RunId;
use run_store::{SlateRunStore, SlateRunStoreInner};

#[derive(Clone)]
pub struct SlateStore {
    object_store: Arc<dyn ObjectStore>,
    base_prefix: String,
    flush_interval: Duration,
    active_runs: Arc<Mutex<HashMap<RunId, std::sync::Weak<SlateRunStoreInner>>>>,
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
            active_runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn repair_catalog(&self) -> Result<()> {
        catalog::repair_catalog(self.object_store.clone(), &self.base_prefix).await
    }

    async fn open_db(&self, db_prefix: &str) -> Result<slatedb::Db> {
        Ok(
            slatedb::Db::builder(db_prefix.to_string(), self.object_store.clone())
                .with_settings(Settings {
                    flush_interval: Some(self.flush_interval),
                    ..Settings::default()
                })
                .build()
                .await?,
        )
    }

    async fn open_reader(&self, db_prefix: &str) -> Result<DbReader> {
        Ok(DbReader::open(
            db_prefix.to_string(),
            self.object_store.clone(),
            None,
            DbReaderOptions {
                manifest_poll_interval: Duration::from_millis(5),
                ..DbReaderOptions::default()
            },
        )
        .await?)
    }

    async fn db_prefix_has_objects(&self, db_prefix: &str) -> Result<bool> {
        let prefix = Path::from(db_prefix.to_string());
        let mut items = self.object_store.list(Some(&prefix));
        Ok(items.try_next().await?.is_some())
    }

    async fn get_active_run(&self, run_id: &RunId) -> Option<SlateRunStore> {
        let mut active_runs = self.active_runs.lock().await;
        let weak = active_runs.get(run_id).cloned()?;
        if let Some(inner) = weak.upgrade() {
            Some(SlateRunStore::from_inner(inner))
        } else {
            active_runs.remove(run_id);
            None
        }
    }

    async fn cache_active_run(&self, run_store: &SlateRunStore) {
        self.active_runs
            .lock()
            .await
            .insert(run_store.record().run_id, run_store.downgrade());
    }

    async fn remove_active_run(&self, run_id: &RunId) -> Option<SlateRunStore> {
        let weak = self.active_runs.lock().await.remove(run_id)?;
        weak.upgrade().map(SlateRunStore::from_inner)
    }

    async fn open_run_store(&self, record: &CatalogRecord) -> Result<Option<SlateRunStore>> {
        if let Some(active) = self.get_active_run(&record.run_id).await {
            if active.matches_record(record) {
                return Ok(Some(active));
            }
            return Err(StoreError::Other(format!(
                "active run cache mismatch for run_id {:?}",
                record.run_id
            )));
        }
        if !self.db_prefix_has_objects(&record.db_prefix).await? {
            return Ok(None);
        }
        let db = self.open_db(&record.db_prefix).await?;
        let has_init = match SlateRunStore::validate_init(&db, record).await {
            Ok(has_init) => has_init,
            Err(err) => {
                let _ = db.close().await;
                return Err(err);
            }
        };
        if !has_init {
            let _ = db.close().await;
            return Ok(None);
        }
        let run_store = SlateRunStore::open_writer(record.clone(), db).await?;
        self.cache_active_run(&run_store).await;
        Ok(Some(run_store))
    }

    async fn open_run_reader_store(&self, record: &CatalogRecord) -> Result<Option<SlateRunStore>> {
        if !self.db_prefix_has_objects(&record.db_prefix).await? {
            return Ok(None);
        }
        let reader = self.open_reader(&record.db_prefix).await?;
        let has_init = match SlateRunStore::validate_init(&reader, record).await {
            Ok(has_init) => has_init,
            Err(err) => {
                let _ = reader.close().await;
                return Err(err);
            }
        };
        if !has_init {
            let _ = reader.close().await;
            return Ok(None);
        }
        SlateRunStore::open_reader(record.clone(), reader)
            .await
            .map(Some)
    }

    async fn delete_db_prefix(&self, db_prefix: &str) -> Result<()> {
        let prefix = Path::from(db_prefix.to_string());
        let metas = self
            .object_store
            .list(Some(&prefix))
            .try_collect::<Vec<_>>()
            .await?;
        for meta in metas {
            delete_path(self.object_store.clone(), &meta.location).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Store for SlateStore {
    async fn create_run(
        &self,
        run_id: &RunId,
        created_at: DateTime<Utc>,
        run_dir: Option<&str>,
    ) -> Result<Arc<dyn RunStore>> {
        let locator =
            catalog::read_locator(self.object_store.clone(), &self.base_prefix, run_id).await?;
        if let Some(active) = self.get_active_run(run_id).await {
            if active.created_at() != created_at
                || locator
                    .as_ref()
                    .is_some_and(|existing| existing.created_at != created_at)
            {
                return Err(StoreError::RunAlreadyExists(run_id.to_string()));
            }
            let record = active.record();
            catalog::write_catalog(
                self.object_store.clone(),
                &self.base_prefix,
                run_id,
                created_at,
                &record.db_prefix,
                run_dir,
            )
            .await?;
            return Ok(Arc::new(active) as Arc<dyn RunStore>);
        }

        let db_prefix = match locator {
            Some(existing) if existing.created_at != created_at => {
                return Err(StoreError::RunAlreadyExists(run_id.to_string()));
            }
            Some(existing) => existing.db_prefix,
            None => catalog::db_prefix(&self.base_prefix, created_at, run_id),
        };

        let record = CatalogRecord {
            run_id: *run_id,
            created_at,
            db_prefix: db_prefix.clone(),
            run_dir: run_dir.map(ToOwned::to_owned),
        };

        let db = self.open_db(&db_prefix).await?;
        SlateRunStore::validate_init(&db, &record).await?;
        db.put(keys::init(), serde_json::to_vec(&record)?).await?;
        let run_store = SlateRunStore::open_writer(record.clone(), db).await?;
        self.cache_active_run(&run_store).await;
        catalog::write_catalog(
            self.object_store.clone(),
            &self.base_prefix,
            run_id,
            created_at,
            &db_prefix,
            run_dir,
        )
        .await?;
        Ok(Arc::new(run_store) as Arc<dyn RunStore>)
    }

    async fn open_run(&self, run_id: &RunId) -> Result<Option<Arc<dyn RunStore>>> {
        let Some(locator) =
            catalog::read_locator(self.object_store.clone(), &self.base_prefix, run_id).await?
        else {
            return Ok(None);
        };

        let Some(run_store) = self.open_run_store(&locator).await? else {
            return Ok(None);
        };
        Ok(Some(Arc::new(run_store) as Arc<dyn RunStore>))
    }

    async fn open_run_reader(&self, run_id: &RunId) -> Result<Option<Arc<dyn RunStore>>> {
        let Some(locator) =
            catalog::read_locator(self.object_store.clone(), &self.base_prefix, run_id).await?
        else {
            return Ok(None);
        };

        let Some(run_store) = self.open_run_reader_store(&locator).await? else {
            return Ok(None);
        };
        Ok(Some(Arc::new(run_store) as Arc<dyn RunStore>))
    }

    async fn list_runs(&self, query: &ListRunsQuery) -> Result<Vec<RunSummary>> {
        let catalogs =
            catalog::list_catalogs(self.object_store.clone(), &self.base_prefix, query).await?;
        let mut summaries = Vec::new();
        for record in catalogs {
            if let Some(active) = self.get_active_run(&record.run_id).await {
                if !active.matches_record(&record) {
                    return Err(StoreError::Other(format!(
                        "active run cache mismatch for run_id {:?}",
                        record.run_id
                    )));
                }
                let snapshot = active.snapshot().await?;
                summaries.push(SlateRunStore::build_summary(snapshot.as_ref(), &record).await?);
                continue;
            }
            if !self.db_prefix_has_objects(&record.db_prefix).await? {
                continue;
            }
            let reader = self.open_reader(&record.db_prefix).await?;
            if !SlateRunStore::validate_init(&reader, &record).await? {
                let _ = reader.close().await;
                continue;
            }
            let summary = SlateRunStore::build_summary(&reader, &record).await;
            let _ = reader.close().await;
            let summary = summary?;
            summaries.push(summary);
        }
        summaries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(summaries)
    }

    async fn delete_run(&self, run_id: &RunId) -> Result<()> {
        let active = self.remove_active_run(run_id).await;
        let active_record = active.as_ref().map(SlateRunStore::record);
        if let Some(active) = &active {
            active.close().await?;
        }

        if let Some(locator) =
            catalog::read_locator(self.object_store.clone(), &self.base_prefix, run_id).await?
        {
            delete_path(
                self.object_store.clone(),
                &catalog::by_start_path(&self.base_prefix, locator.created_at, run_id),
            )
            .await?;
            self.delete_db_prefix(&locator.db_prefix).await?;
            delete_path(
                self.object_store.clone(),
                &catalog::by_id_path(&self.base_prefix, run_id),
            )
            .await?;
            return Ok(());
        }

        if let Some(record) = active_record {
            delete_path(
                self.object_store.clone(),
                &catalog::by_start_path(&self.base_prefix, record.created_at, run_id),
            )
            .await?;
            self.delete_db_prefix(&record.db_prefix).await?;
            delete_path(
                self.object_store.clone(),
                &catalog::by_id_path(&self.base_prefix, run_id),
            )
            .await?;
            return Ok(());
        }

        let by_start_prefix = Path::from(format!("{}by-start", self.base_prefix));
        let metas = self
            .object_store
            .list(Some(&by_start_prefix))
            .try_collect::<Vec<_>>()
            .await?;
        let expected_name = format!("{run_id}.json");
        for meta in metas {
            if meta.location.filename() != Some(expected_name.as_str()) {
                continue;
            }
            let Some(record) =
                catalog::read_catalog_path(self.object_store.clone(), meta.location.clone())
                    .await?
            else {
                delete_path(self.object_store.clone(), &meta.location).await?;
                continue;
            };
            self.delete_db_prefix(&record.db_prefix).await?;
            delete_path(self.object_store.clone(), &meta.location).await?;
        }
        Ok(())
    }
}

async fn delete_path(store: Arc<dyn ObjectStore>, path: &Path) -> Result<()> {
    match store.delete(path).await {
        Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
        Err(err) => Err(err.into()),
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

    use std::path::PathBuf;
    use std::time::Duration;

    use bytes::Bytes;
    use fabro_types::{
        AttrValue, Checkpoint, Conclusion, FabroSettings, Graph, NodeStatusRecord, RunId,
        RunRecord, RunStatus, RunStatusRecord, StageStatus, StartRecord, StatusReason, fixtures,
    };
    use object_store::memory::InMemory;
    use slatedb::config::Settings;
    use slatedb::{CloseReason, ErrorKind};
    use tokio::time::timeout;

    use crate::{EventPayload, NodeVisitRef};

    fn dt(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn make_store() -> (Arc<dyn ObjectStore>, SlateStore) {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = SlateStore::new(object_store.clone(), "runs/", Duration::from_millis(1));
        (object_store, store)
    }

    fn test_run_id(label: &str) -> RunId {
        match label {
            "run-1" => fixtures::RUN_1,
            "other-run" => fixtures::RUN_2,
            _ => panic!("unknown test run id: {label}"),
        }
    }

    fn sample_run_record(run_id: &str, created_at: DateTime<Utc>) -> RunRecord {
        let mut graph = Graph::new("night-sky");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("map the constellations".to_string()),
        );
        RunRecord {
            run_id: test_run_id(run_id),
            created_at,
            settings: FabroSettings::default(),
            graph,
            workflow_slug: Some("night-sky".to_string()),
            working_directory: PathBuf::from("/tmp/night-sky"),
            host_repo_path: Some("github.com/fabro-sh/fabro".to_string()),
            base_branch: Some("main".to_string()),
            labels: std::collections::HashMap::from([("team".to_string(), "infra".to_string())]),
        }
    }

    fn sample_start_record(run_id: &str, created_at: DateTime<Utc>) -> StartRecord {
        StartRecord {
            run_id: test_run_id(run_id),
            start_time: created_at + chrono::Duration::seconds(5),
            run_branch: Some("fabro/run/demo".to_string()),
            base_sha: Some("abc123".to_string()),
        }
    }

    fn sample_status(status: RunStatus, reason: Option<StatusReason>) -> RunStatusRecord {
        RunStatusRecord {
            status,
            reason,
            updated_at: dt("2026-03-27T12:05:00Z"),
        }
    }

    fn sample_checkpoint() -> Checkpoint {
        Checkpoint {
            timestamp: dt("2026-03-27T12:10:00Z"),
            current_node: "code".to_string(),
            completed_nodes: vec!["plan".to_string()],
            node_retries: std::collections::HashMap::from([("code".to_string(), 1)]),
            context_values: std::collections::HashMap::new(),
            node_outcomes: std::collections::HashMap::new(),
            next_node_id: Some("review".to_string()),
            git_commit_sha: Some("def456".to_string()),
            loop_failure_signatures: std::collections::HashMap::new(),
            restart_failure_signatures: std::collections::HashMap::new(),
            node_visits: std::collections::HashMap::from([("code".to_string(), 2)]),
        }
    }

    fn sample_conclusion() -> Conclusion {
        Conclusion {
            timestamp: dt("2026-03-27T12:15:00Z"),
            status: StageStatus::Success,
            duration_ms: 3210,
            failure_reason: None,
            final_git_commit_sha: Some("feedbeef".to_string()),
            stages: Vec::new(),
            total_cost: Some(1.25),
            total_retries: 2,
            total_input_tokens: 10,
            total_output_tokens: 20,
            total_cache_read_tokens: 30,
            total_cache_write_tokens: 40,
            total_reasoning_tokens: 50,
            has_pricing: true,
        }
    }

    fn sample_node_status() -> NodeStatusRecord {
        NodeStatusRecord {
            status: StageStatus::Success,
            notes: Some("done".to_string()),
            failure_reason: None,
            timestamp: dt("2026-03-27T12:12:00Z"),
        }
    }

    fn event_payload(run_id: &str, ts: &str, event: &str) -> EventPayload {
        EventPayload::new(
            serde_json::json!({
                "id": format!("evt-{run_id}-{event}"),
                "ts": ts,
                "run_id": test_run_id(run_id).to_string(),
                "event": event
            }),
            &test_run_id(run_id),
        )
        .unwrap()
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

    async fn object_exists(store: Arc<dyn ObjectStore>, path: &Path) -> bool {
        store.head(path).await.is_ok()
    }

    async fn seed_db(
        object_store: Arc<dyn ObjectStore>,
        record: &CatalogRecord,
        include_init: bool,
    ) -> slatedb::Db {
        let db = slatedb::Db::builder(record.db_prefix.clone(), object_store)
            .with_settings(Settings {
                flush_interval: Some(Duration::from_millis(1)),
                ..Settings::default()
            })
            .build()
            .await
            .unwrap();
        if include_init {
            db.put(keys::init(), serde_json::to_vec(record).unwrap())
                .await
                .unwrap();
        }
        db
    }

    #[tokio::test]
    async fn create_open_list_and_delete_full_lifecycle() {
        let (object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();

        run.put_run(&sample_run_record("run-1", created_at))
            .await
            .unwrap();
        run.put_start(&sample_start_record("run-1", created_at))
            .await
            .unwrap();
        run.put_status(&sample_status(
            RunStatus::Succeeded,
            Some(StatusReason::Completed),
        ))
        .await
        .unwrap();
        run.put_conclusion(&sample_conclusion()).await.unwrap();

        let by_id = catalog::by_id_path("runs/", &test_run_id("run-1"));
        let by_start = catalog::by_start_path("runs/", created_at, &test_run_id("run-1"));
        assert!(object_exists(object_store.clone(), &by_id).await);
        assert!(object_exists(object_store.clone(), &by_start).await);

        let summary = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].run_id, test_run_id("run-1"));
        assert_eq!(summary[0].workflow_name, Some("night-sky".to_string()));
        assert_eq!(summary[0].goal, Some("map the constellations".to_string()));
        assert_eq!(summary[0].status, Some(RunStatus::Succeeded));
        assert_eq!(summary[0].status_reason, Some(StatusReason::Completed));

        let reopened = store
            .open_run(&test_run_id("run-1"))
            .await
            .unwrap()
            .unwrap();
        let stored = reopened.get_run().await.unwrap().unwrap();
        assert_eq!(stored.run_id, test_run_id("run-1"));

        store.delete_run(&test_run_id("run-1")).await.unwrap();
        assert!(
            store
                .open_run(&test_run_id("run-1"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(!object_exists(object_store.clone(), &by_id).await);
        assert!(!object_exists(object_store.clone(), &by_start).await);
        assert!(list_paths(object_store, "runs/db").await.is_empty());
    }

    #[tokio::test]
    async fn by_id_without_by_start_opens_but_is_omitted_from_list_and_repair_restores_index() {
        let (object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let record = CatalogRecord {
            run_id: test_run_id("run-1"),
            created_at,
            db_prefix: catalog::db_prefix("runs/", created_at, &test_run_id("run-1")),
            run_dir: None,
        };

        let db = seed_db(object_store.clone(), &record, true).await;
        db.put(
            keys::run(),
            serde_json::to_vec(&sample_run_record("run-1", created_at)).unwrap(),
        )
        .await
        .unwrap();
        db.close().await.unwrap();

        object_store
            .put(
                &catalog::by_id_path("runs/", &test_run_id("run-1")),
                serde_json::to_vec(&record).unwrap().into(),
            )
            .await
            .unwrap();

        assert!(
            store
                .open_run(&test_run_id("run-1"))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .list_runs(&ListRunsQuery::default())
                .await
                .unwrap()
                .is_empty()
        );

        store.repair_catalog().await.unwrap();
        let listed = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(
            object_exists(
                object_store,
                &catalog::by_start_path("runs/", created_at, &test_run_id("run-1"))
            )
            .await
        );
    }

    #[tokio::test]
    async fn reopen_recovers_event_and_checkpoint_sequences() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        run.put_run(&sample_run_record("run-1", created_at))
            .await
            .unwrap();
        run.append_event(&event_payload("run-1", "2026-03-27T12:00:00Z", "Started"))
            .await
            .unwrap();
        run.append_event(&event_payload("run-1", "2026-03-27T12:00:01Z", "Next"))
            .await
            .unwrap();
        run.append_checkpoint(&sample_checkpoint()).await.unwrap();
        drop(run);

        let reopened = store
            .open_run(&test_run_id("run-1"))
            .await
            .unwrap()
            .unwrap();
        let next_event = reopened
            .append_event(&event_payload(
                "run-1",
                "2026-03-27T12:00:02Z",
                "AfterReopen",
            ))
            .await
            .unwrap();
        let next_checkpoint = reopened
            .append_checkpoint(&sample_checkpoint())
            .await
            .unwrap();
        assert_eq!(next_event, 3);
        assert_eq!(next_checkpoint, 2);
    }

    #[tokio::test]
    async fn open_run_and_list_runs_skip_empty_databases_without_init() {
        let (object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let record = CatalogRecord {
            run_id: test_run_id("run-1"),
            created_at,
            db_prefix: catalog::db_prefix("runs/", created_at, &test_run_id("run-1")),
            run_dir: None,
        };

        let db = seed_db(object_store.clone(), &record, false).await;
        db.close().await.unwrap();
        object_store
            .put(
                &catalog::by_id_path("runs/", &test_run_id("run-1")),
                serde_json::to_vec(&record).unwrap().into(),
            )
            .await
            .unwrap();
        object_store
            .put(
                &catalog::by_start_path("runs/", created_at, &test_run_id("run-1")),
                serde_json::to_vec(&record).unwrap().into(),
            )
            .await
            .unwrap();

        assert!(
            store
                .open_run(&test_run_id("run-1"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .list_runs(&ListRunsQuery::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn create_run_allows_idempotent_retry_and_rejects_conflict() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();

        let conflict = store
            .create_run(
                &test_run_id("run-1"),
                created_at + chrono::Duration::seconds(1),
                None,
            )
            .await;
        assert!(matches!(conflict, Err(StoreError::RunAlreadyExists(_))));
    }

    #[tokio::test]
    async fn list_runs_and_open_run_reuse_active_handle_without_fencing() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        run.put_run(&sample_run_record("run-1", created_at))
            .await
            .unwrap();

        let listed = store.list_runs(&ListRunsQuery::default()).await.unwrap();
        assert_eq!(listed.len(), 1);

        let reopened = store
            .open_run(&test_run_id("run-1"))
            .await
            .unwrap()
            .unwrap();
        let first_event = run
            .append_event(&event_payload("run-1", "2026-03-27T12:00:00Z", "Started"))
            .await
            .unwrap();
        let second_event = reopened
            .append_event(&event_payload("run-1", "2026-03-27T12:00:01Z", "Continued"))
            .await
            .unwrap();
        let first_checkpoint = run.append_checkpoint(&sample_checkpoint()).await.unwrap();
        let second_checkpoint = reopened
            .append_checkpoint(&sample_checkpoint())
            .await
            .unwrap();

        assert_eq!(first_event, 1);
        assert_eq!(second_event, 2);
        assert_eq!(first_checkpoint, 1);
        assert_eq!(second_checkpoint, 2);
    }

    #[tokio::test]
    async fn watch_events_from_polls_new_events() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        let mut stream = run.watch_events_from(1).await.unwrap();

        run.append_event(&event_payload("run-1", "2026-03-27T12:00:00Z", "Started"))
            .await
            .unwrap();

        let event = timeout(
            Duration::from_secs(2),
            futures::StreamExt::next(&mut stream),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(event.seq, 1);
    }

    #[tokio::test]
    async fn delete_run_is_idempotent_and_fallback_cleans_by_start_orphans() {
        let (object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let record = CatalogRecord {
            run_id: test_run_id("run-1"),
            created_at,
            db_prefix: catalog::db_prefix("runs/", created_at, &test_run_id("run-1")),
            run_dir: None,
        };
        let db = seed_db(object_store.clone(), &record, true).await;
        db.put(
            keys::run(),
            serde_json::to_vec(&sample_run_record("run-1", created_at)).unwrap(),
        )
        .await
        .unwrap();
        db.close().await.unwrap();
        object_store
            .put(
                &catalog::by_start_path("runs/", created_at, &test_run_id("run-1")),
                serde_json::to_vec(&record).unwrap().into(),
            )
            .await
            .unwrap();

        store.delete_run(&test_run_id("run-1")).await.unwrap();
        store.delete_run(&test_run_id("run-1")).await.unwrap();
        assert!(list_paths(object_store, "runs").await.is_empty());
    }

    #[tokio::test]
    async fn delete_run_closes_active_handles() {
        let (object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        run.put_run(&sample_run_record("run-1", created_at))
            .await
            .unwrap();

        store.delete_run(&test_run_id("run-1")).await.unwrap();

        let err = run.put_graph("digraph night_sky {}").await.unwrap_err();
        assert!(matches!(
            err,
            StoreError::Slate(err) if matches!(err.kind(), ErrorKind::Closed(CloseReason::Clean))
        ));
        assert!(
            store
                .open_run(&test_run_id("run-1"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(list_paths(object_store, "runs").await.is_empty());
    }

    #[tokio::test]
    async fn repair_catalog_removes_stale_wrong_time_prefixes() {
        let (object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let wrong_time = dt("2026-03-27T11:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        run.put_run(&sample_run_record("run-1", created_at))
            .await
            .unwrap();

        let locator = catalog::read_locator(object_store.clone(), "runs/", &test_run_id("run-1"))
            .await
            .unwrap()
            .unwrap();
        object_store
            .put(
                &catalog::by_start_path("runs/", wrong_time, &test_run_id("run-1")),
                serde_json::to_vec(&locator).unwrap().into(),
            )
            .await
            .unwrap();

        store.repair_catalog().await.unwrap();
        let paths = list_paths(object_store, "runs/by-start").await;
        assert_eq!(paths.len(), 1);
        assert!(paths[0].contains(&format!("2026-03-27-12-00/{}.json", test_run_id("run-1"))));
    }

    #[tokio::test]
    async fn create_run_uses_distinct_db_prefix_for_same_minute_orphan() {
        let (object_store, store) = make_store();
        let old_created_at = dt("2026-03-27T12:00:00Z");
        let new_created_at = dt("2026-03-27T12:00:30Z");
        let orphan = CatalogRecord {
            run_id: test_run_id("run-1"),
            created_at: old_created_at,
            db_prefix: catalog::db_prefix("runs/", old_created_at, &test_run_id("run-1")),
            run_dir: None,
        };
        let new_prefix = catalog::db_prefix("runs/", new_created_at, &test_run_id("run-1"));
        assert_ne!(orphan.db_prefix, new_prefix);

        let db = seed_db(object_store.clone(), &orphan, true).await;
        db.put(keys::graph(), b"stale graph").await.unwrap();
        db.close().await.unwrap();

        let run = store
            .create_run(&test_run_id("run-1"), new_created_at, None)
            .await
            .unwrap();
        assert_eq!(run.get_graph().await.unwrap(), None);

        let locator = catalog::read_locator(object_store, "runs/", &test_run_id("run-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(locator.created_at, new_created_at);
        assert_eq!(locator.db_prefix, new_prefix);
    }

    #[tokio::test]
    async fn create_run_rejects_mismatched_init_for_existing_prefix() {
        let (object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let db_prefix = catalog::db_prefix("runs/", created_at, &test_run_id("run-1"));
        let db = slatedb::Db::builder(db_prefix.clone(), object_store)
            .with_settings(Settings {
                flush_interval: Some(Duration::from_millis(1)),
                ..Settings::default()
            })
            .build()
            .await
            .unwrap();
        let mismatched = CatalogRecord {
            run_id: test_run_id("other-run"),
            created_at,
            db_prefix,
            run_dir: None,
        };
        db.put(keys::init(), serde_json::to_vec(&mismatched).unwrap())
            .await
            .unwrap();
        db.close().await.unwrap();

        let Err(err) = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
        else {
            panic!("expected create_run to reject mismatched _init.json");
        };
        assert!(matches!(
            err,
            StoreError::Other(message) if message.contains("_init.json")
        ));
    }

    #[tokio::test]
    async fn slate_run_store_round_trips_node_data_and_assets() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        run.put_run(&sample_run_record("run-1", created_at))
            .await
            .unwrap();
        let node = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        run.put_node_prompt(&node, "Plan").await.unwrap();
        run.put_node_status(&node, &sample_node_status())
            .await
            .unwrap();
        run.put_asset(&node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let snapshot = run.get_node(&node).await.unwrap();
        assert_eq!(snapshot.prompt, Some("Plan".to_string()));
        assert_eq!(
            run.get_asset(&node, "src/lib.rs").await.unwrap(),
            Some(Bytes::from_static(b"fn main() {}"))
        );
        assert_eq!(
            run.list_assets(&node).await.unwrap(),
            vec!["src/lib.rs".to_string()]
        );
    }

    #[tokio::test]
    async fn slate_run_store_lists_artifact_values_and_asset_only_visits() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        run.put_run(&sample_run_record("run-1", created_at))
            .await
            .unwrap();

        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
            .await
            .unwrap();
        run.put_artifact_value("plan", &serde_json::json!({"steps": 3}))
            .await
            .unwrap();

        let snapshot_node = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        run.put_node_prompt(&snapshot_node, "Plan").await.unwrap();
        run.put_asset(&snapshot_node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let asset_only_node = NodeVisitRef {
            node_id: "artifact-only",
            visit: 7,
        };
        run.put_asset(&asset_only_node, "logs/output.txt", b"hello")
            .await
            .unwrap();

        assert_eq!(
            run.list_artifact_values().await.unwrap(),
            vec!["plan".to_string(), "summary".to_string()]
        );
        assert_eq!(
            run.list_all_assets().await.unwrap(),
            vec![
                (
                    "artifact-only".to_string(),
                    7,
                    "logs/output.txt".to_string()
                ),
                ("code".to_string(), 2, "src/lib.rs".to_string())
            ]
        );

        let snapshot = run.get_snapshot().await.unwrap().unwrap();
        assert_eq!(snapshot.nodes.len(), 1);
        assert_eq!(snapshot.nodes[0].node_id, "code");
    }
}
