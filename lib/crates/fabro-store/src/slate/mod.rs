mod catalog;
mod run_store;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use object_store::ObjectStore;
use object_store::path::Path;
use slatedb::DbReader;
use slatedb::config::{DbReaderOptions, Settings};
use tokio::sync::Mutex;

use crate::keys;
use crate::{CatalogRecord, ListRunsQuery, Result, RunSummary, StoreError};
use fabro_types::RunId;
use run_store::SlateRunStoreInner;
pub use run_store::{NodeAsset, SlateRunStore};

#[derive(Clone, Copy)]
enum RunOpenMode {
    Writer,
    Reader,
}

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

    async fn open_existing_run(
        &self,
        record: &CatalogRecord,
        mode: RunOpenMode,
    ) -> Result<Option<SlateRunStore>> {
        if matches!(mode, RunOpenMode::Writer) {
            if let Some(active) = self.get_active_run(&record.run_id).await {
                if active.record() == *record {
                    return Ok(Some(active));
                }
                return Err(StoreError::Other(format!(
                    "active run cache mismatch for run_id {:?}",
                    record.run_id
                )));
            }
        }
        if !self.db_prefix_has_objects(&record.db_prefix).await? {
            return Ok(None);
        }
        match mode {
            RunOpenMode::Writer => {
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
            RunOpenMode::Reader => {
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
        }
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

impl SlateStore {
    pub async fn create_run(
        &self,
        run_id: &RunId,
        created_at: DateTime<Utc>,
        run_dir: Option<&str>,
    ) -> Result<SlateRunStore> {
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
            return Ok(active);
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
        Ok(run_store)
    }

    pub async fn open_run(&self, run_id: &RunId) -> Result<SlateRunStore> {
        let locator = catalog::read_locator(self.object_store.clone(), &self.base_prefix, run_id)
            .await?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;

        let run_store = self
            .open_existing_run(&locator, RunOpenMode::Writer)
            .await?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        Ok(run_store)
    }

    pub async fn open_run_reader(&self, run_id: &RunId) -> Result<SlateRunStore> {
        let locator = catalog::read_locator(self.object_store.clone(), &self.base_prefix, run_id)
            .await?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;

        let run_store = self
            .open_existing_run(&locator, RunOpenMode::Reader)
            .await?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        Ok(run_store)
    }

    pub async fn list_runs(&self) -> Result<Vec<RunSummary>> {
        self.list_runs_in_range(None, None).await
    }

    pub async fn list_runs_in_range(
        &self,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> Result<Vec<RunSummary>> {
        let query = ListRunsQuery { start, end };
        let catalogs =
            catalog::list_catalogs(self.object_store.clone(), &self.base_prefix, &query).await?;
        let mut summaries = Vec::new();
        for record in catalogs {
            if let Some(active) = self.get_active_run(&record.run_id).await {
                if active.record() != record {
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
        summaries.sort_by(|a, b| b.catalog.created_at.cmp(&a.catalog.created_at));
        Ok(summaries)
    }

    pub async fn delete_run(&self, run_id: &RunId) -> Result<()> {
        let active = self.remove_active_run(run_id).await;
        let active_record = active.as_ref().map(SlateRunStore::record);
        if let Some(active) = &active {
            active.close().await?;
        }

        if let Some(record) =
            catalog::read_locator(self.object_store.clone(), &self.base_prefix, run_id)
                .await?
                .or(active_record)
        {
            return self.delete_run_record(&record).await;
        }

        self.repair_delete_run(run_id).await
    }
}

impl SlateStore {
    async fn delete_run_record(&self, record: &CatalogRecord) -> Result<()> {
        delete_path(
            self.object_store.clone(),
            &catalog::by_start_path(&self.base_prefix, record.created_at, &record.run_id),
        )
        .await?;
        self.delete_db_prefix(&record.db_prefix).await?;
        delete_path(
            self.object_store.clone(),
            &catalog::by_id_path(&self.base_prefix, &record.run_id),
        )
        .await?;
        Ok(())
    }

    async fn repair_delete_run(&self, run_id: &RunId) -> Result<()> {
        let by_start_prefix = Path::from(format!("{}by-start", self.base_prefix));
        let db_prefix = Path::from(format!("{}db", self.base_prefix));
        let expected_name = format!("{run_id}.json");

        for meta in self
            .object_store
            .list(Some(&by_start_prefix))
            .try_collect::<Vec<_>>()
            .await?
        {
            if meta.location.filename() == Some(expected_name.as_str()) {
                delete_path(self.object_store.clone(), &meta.location).await?;
            }
        }

        let db_run_segment = format!("/{run_id}/");
        for meta in self
            .object_store
            .list(Some(&db_prefix))
            .try_collect::<Vec<_>>()
            .await?
        {
            if meta.location.to_string().contains(&db_run_segment) {
                delete_path(self.object_store.clone(), &meta.location).await?;
            }
        }

        delete_path(
            self.object_store.clone(),
            &catalog::by_id_path(&self.base_prefix, run_id),
        )
        .await?;
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

    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use bytes::Bytes;
    use chrono::Duration as ChronoDuration;
    use fabro_types::{
        AttrValue, Checkpoint, Conclusion, Graph, PullRequestRecord, Retro, RunId, RunRecord,
        RunStatus, RunStatusRecord, SandboxRecord, Settings, StageStatus, StartRecord,
        StatusReason, fixtures,
    };
    use object_store::memory::InMemory;
    use slatedb::config::Settings as SlateSettings;
    use slatedb::{CloseReason, ErrorKind};
    use tokio::time::timeout;

    use crate::{EventPayload, StageId};

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

    async fn repair_catalog_for_tests(store: &SlateStore) -> Result<()> {
        catalog::test_support::repair_catalog(store.object_store.clone(), &store.base_prefix).await
    }

    fn test_run_id(label: &str) -> RunId {
        match label {
            "run-1" => fixtures::RUN_1,
            "other-run" => fixtures::RUN_2,
            "run-early" => fixtures::RUN_2,
            "run-late" => fixtures::RUN_3,
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
            settings: Settings::default(),
            graph,
            workflow_slug: Some("night-sky".to_string()),
            working_directory: PathBuf::from("/tmp/night-sky"),
            host_repo_path: Some("github.com/fabro-sh/fabro".to_string()),
            base_branch: Some("main".to_string()),
            labels: std::collections::HashMap::from([("team".to_string(), "infra".to_string())]),
        }
    }

    fn event_payload(
        run_id: &str,
        ts: &str,
        event: &str,
        node_id: Option<&str>,
        properties: serde_json::Value,
    ) -> EventPayload {
        let mut value = serde_json::json!({
            "id": format!("evt-{run_id}-{event}"),
            "ts": ts,
            "run_id": test_run_id(run_id).to_string(),
            "event": event,
            "properties": properties,
        });
        if let Some(node_id) = node_id {
            value["node_id"] = serde_json::Value::String(node_id.to_string());
        }
        EventPayload::new(value, &test_run_id(run_id)).unwrap()
    }

    fn sample_start_record(run_id: &str, created_at: DateTime<Utc>) -> StartRecord {
        StartRecord {
            run_id: test_run_id(run_id),
            start_time: created_at + ChronoDuration::seconds(5),
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
            node_retries: HashMap::from([("code".to_string(), 1)]),
            context_values: HashMap::from([(
                "artifact".to_string(),
                serde_json::json!({"kind": "summary"}),
            )]),
            node_outcomes: HashMap::new(),
            next_node_id: Some("review".to_string()),
            git_commit_sha: Some("def456".to_string()),
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits: HashMap::from([("code".to_string(), 2)]),
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

    fn sample_retro(run_id: &str) -> Retro {
        Retro {
            run_id: test_run_id(run_id),
            workflow_name: "night-sky".to_string(),
            goal: "map the constellations".to_string(),
            timestamp: dt("2026-03-27T12:20:00Z"),
            smoothness: None,
            stages: Vec::new(),
            stats: fabro_types::AggregateStats {
                total_duration_ms: 3210,
                total_cost: Some(1.25),
                total_retries: 2,
                files_touched: vec!["src/lib.rs".to_string()],
                stages_completed: 3,
                stages_failed: 0,
            },
            intent: Some("ship the fix".to_string()),
            outcome: Some("done".to_string()),
            learnings: None,
            friction_points: None,
            open_items: None,
        }
    }

    fn sample_sandbox() -> SandboxRecord {
        SandboxRecord {
            provider: "local".to_string(),
            working_directory: "/tmp/night-sky".to_string(),
            identifier: Some("sandbox-1".to_string()),
            host_working_directory: Some("/tmp/night-sky".to_string()),
            container_mount_point: None,
        }
    }

    fn sample_pull_request() -> PullRequestRecord {
        PullRequestRecord {
            html_url: "https://github.com/fabro-sh/fabro/pull/123".to_string(),
            number: 123,
            owner: "fabro-sh".to_string(),
            repo: "fabro".to_string(),
            base_branch: "main".to_string(),
            head_branch: "fabro/run/demo".to_string(),
            title: "Map the constellations".to_string(),
        }
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

    async fn read_json_value(store: Arc<dyn ObjectStore>, path: &Path) -> serde_json::Value {
        let result = store.get(path).await.unwrap();
        serde_json::from_slice(&result.bytes().await.unwrap()).unwrap()
    }

    async fn seed_db(
        object_store: Arc<dyn ObjectStore>,
        record: &CatalogRecord,
        include_init: bool,
    ) -> slatedb::Db {
        let db = slatedb::Db::builder(record.db_prefix.clone(), object_store)
            .with_settings(SlateSettings {
                flush_interval: Some(Duration::from_millis(1)),
                ..SlateSettings::default()
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

        let run_record = sample_run_record("run-1", created_at);
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": run_record.settings,
                "graph": run_record.graph,
                "workflow_slug": run_record.workflow_slug,
                "working_directory": run_record.working_directory,
                "host_repo_path": run_record.host_repo_path,
                "base_branch": run_record.base_branch,
                "labels": run_record.labels,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:01Z",
            "run.started",
            None,
            serde_json::json!({
                "run_branch": "fabro/run/demo",
                "base_sha": "abc123",
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "run.completed",
            None,
            serde_json::json!({
                "duration_ms": 3210,
                "artifact_count": 1,
                "status": "success",
                "reason": "completed",
                "total_cost": 1.25,
            }),
        ))
        .await
        .unwrap();

        let by_id = catalog::by_id_path("runs/", &test_run_id("run-1"));
        let by_start = catalog::by_start_path("runs/", created_at, &test_run_id("run-1"));
        assert!(object_exists(object_store.clone(), &by_id).await);
        assert!(object_exists(object_store.clone(), &by_start).await);
        assert_eq!(
            read_json_value(object_store.clone(), &by_start).await,
            serde_json::json!({
                "run_id": test_run_id("run-1").to_string(),
            })
        );

        let summary = store.list_runs().await.unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].catalog.run_id, test_run_id("run-1"));
        assert_eq!(summary[0].workflow_name, Some("night-sky".to_string()));
        assert_eq!(summary[0].goal, Some("map the constellations".to_string()));
        assert_eq!(summary[0].status, Some(RunStatus::Succeeded));
        assert_eq!(summary[0].status_reason, Some(StatusReason::Completed));

        let reopened = store.open_run(&test_run_id("run-1")).await.unwrap();
        let stored = reopened.state().await.unwrap().run.unwrap();
        assert_eq!(stored.run_id, test_run_id("run-1"));

        store.delete_run(&test_run_id("run-1")).await.unwrap();
        assert!(store.open_run(&test_run_id("run-1")).await.is_err());
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
            keys::event_key(1, created_at.timestamp_millis()),
            serde_json::to_vec(&event_payload(
                "run-1",
                "2026-03-27T12:00:00Z",
                "run.created",
                None,
                serde_json::json!({
                    "settings": sample_run_record("run-1", created_at).settings,
                    "graph": sample_run_record("run-1", created_at).graph,
                    "workflow_slug": sample_run_record("run-1", created_at).workflow_slug,
                    "working_directory": sample_run_record("run-1", created_at).working_directory,
                    "host_repo_path": sample_run_record("run-1", created_at).host_repo_path,
                    "base_branch": sample_run_record("run-1", created_at).base_branch,
                    "labels": sample_run_record("run-1", created_at).labels,
                }),
            ))
            .unwrap(),
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

        assert!(store.open_run(&test_run_id("run-1")).await.is_ok());
        assert!(store.list_runs().await.unwrap().is_empty());

        repair_catalog_for_tests(&store).await.unwrap();
        let listed = store.list_runs().await.unwrap();
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
    async fn list_runs_uses_by_start_index_to_resolve_canonical_by_id_record() {
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
            keys::event_key(1, created_at.timestamp_millis()),
            serde_json::to_vec(&event_payload(
                "run-1",
                "2026-03-27T12:00:00Z",
                "run.created",
                None,
                serde_json::json!({
                    "settings": sample_run_record("run-1", created_at).settings,
                    "graph": sample_run_record("run-1", created_at).graph,
                    "workflow_slug": sample_run_record("run-1", created_at).workflow_slug,
                    "working_directory": sample_run_record("run-1", created_at).working_directory,
                    "host_repo_path": sample_run_record("run-1", created_at).host_repo_path,
                    "base_branch": sample_run_record("run-1", created_at).base_branch,
                    "labels": sample_run_record("run-1", created_at).labels,
                }),
            ))
            .unwrap(),
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
        object_store
            .put(
                &catalog::by_start_path("runs/", created_at, &test_run_id("run-1")),
                serde_json::json!({
                    "run_id": test_run_id("run-1").to_string(),
                })
                .to_string()
                .into(),
            )
            .await
            .unwrap();

        let listed = store.list_runs().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].catalog, record);
    }

    #[tokio::test]
    async fn reopen_recovers_event_sequences() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        let run_record = sample_run_record("run-1", created_at);
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": run_record.settings,
                "graph": run_record.graph,
                "workflow_slug": run_record.workflow_slug,
                "working_directory": run_record.working_directory,
                "host_repo_path": run_record.host_repo_path,
                "base_branch": run_record.base_branch,
                "labels": run_record.labels,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:01Z",
            "Started",
            None,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "Next",
            None,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
        drop(run);

        let reopened = store.open_run(&test_run_id("run-1")).await.unwrap();
        let next_event = reopened
            .append_event(&event_payload(
                "run-1",
                "2026-03-27T12:00:02Z",
                "AfterReopen",
                None,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(next_event, 4);
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

        assert!(store.open_run(&test_run_id("run-1")).await.is_err());
        assert!(store.list_runs().await.unwrap().is_empty());
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
        let run_record = sample_run_record("run-1", created_at);
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": run_record.settings,
                "graph": run_record.graph,
                "workflow_slug": run_record.workflow_slug,
                "working_directory": run_record.working_directory,
                "host_repo_path": run_record.host_repo_path,
                "base_branch": run_record.base_branch,
                "labels": run_record.labels,
            }),
        ))
        .await
        .unwrap();

        let listed = store.list_runs().await.unwrap();
        assert_eq!(listed.len(), 1);

        let reopened = store.open_run(&test_run_id("run-1")).await.unwrap();
        let first_event = run
            .append_event(&event_payload(
                "run-1",
                "2026-03-27T12:00:00Z",
                "Started",
                None,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        let second_event = reopened
            .append_event(&event_payload(
                "run-1",
                "2026-03-27T12:00:01Z",
                "Continued",
                None,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(first_event, 2);
        assert_eq!(second_event, 3);
    }

    #[tokio::test]
    async fn watch_events_from_polls_new_events() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        let mut stream = run.watch_events_from(1).unwrap();

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "Started",
            None,
            serde_json::json!({}),
        ))
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
            keys::event_key(1, created_at.timestamp_millis()),
            serde_json::to_vec(&event_payload(
                "run-1",
                "2026-03-27T12:00:00Z",
                "run.created",
                None,
                serde_json::json!({
                    "settings": sample_run_record("run-1", created_at).settings,
                    "graph": sample_run_record("run-1", created_at).graph,
                    "workflow_slug": sample_run_record("run-1", created_at).workflow_slug,
                    "working_directory": sample_run_record("run-1", created_at).working_directory,
                    "host_repo_path": sample_run_record("run-1", created_at).host_repo_path,
                    "base_branch": sample_run_record("run-1", created_at).base_branch,
                    "labels": sample_run_record("run-1", created_at).labels,
                }),
            ))
            .unwrap(),
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
        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
            .await
            .unwrap();

        store.delete_run(&test_run_id("run-1")).await.unwrap();

        let err = run
            .put_artifact_value("summary", &serde_json::json!({"done": false}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::Slate(err) if matches!(err.kind(), ErrorKind::Closed(CloseReason::Clean))
        ));
        assert!(store.open_run(&test_run_id("run-1")).await.is_err());
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
        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
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

        repair_catalog_for_tests(&store).await.unwrap();
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
        db.close().await.unwrap();

        let run = store
            .create_run(&test_run_id("run-1"), new_created_at, None)
            .await
            .unwrap();
        assert!(run.state().await.unwrap().graph_source.is_none());

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
            .with_settings(SlateSettings {
                flush_interval: Some(Duration::from_millis(1)),
                ..SlateSettings::default()
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
    async fn slate_run_store_round_trips_assets_and_projects_events() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        let node = StageId::new("code", 2);
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:01:00Z",
            "stage.prompt",
            Some("code"),
            serde_json::json!({"text": "Plan", "visit": 2}),
        ))
        .await
        .unwrap();
        run.put_asset(&node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let state = run.state().await.unwrap();
        let node_state = state.node(&node).unwrap();
        assert_eq!(node_state.prompt, Some("Plan".to_string()));
        assert_eq!(
            run.get_asset(&node, "src/lib.rs").await.unwrap(),
            Some(Bytes::from_static(b"fn main() {}"))
        );
    }

    #[tokio::test]
    async fn slate_run_store_lists_artifact_values_and_assets() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
            .await
            .unwrap();
        run.put_artifact_value("plan", &serde_json::json!({"steps": 3}))
            .await
            .unwrap();

        let snapshot_node = StageId::new("code", 2);
        run.put_asset(&snapshot_node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let asset_only_node = StageId::new("artifact-only", 7);
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
                crate::slate::NodeAsset {
                    node: crate::StageId::new("artifact-only", 7),
                    filename: "logs/output.txt".to_string(),
                },
                crate::slate::NodeAsset {
                    node: crate::StageId::new("code", 2),
                    filename: "src/lib.rs".to_string(),
                }
            ]
        );
    }

    #[tokio::test]
    async fn create_run_state_and_node_storage_round_trip() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();

        let run_record = sample_run_record("run-1", created_at);
        let start_record = sample_start_record("run-1", created_at);
        let status_record =
            sample_status(RunStatus::Running, Some(StatusReason::SandboxInitializing));
        let checkpoint = sample_checkpoint();
        let conclusion = sample_conclusion();
        let retro = sample_retro("run-1");
        let sandbox = sample_sandbox();
        let node = StageId::new("code", 2);
        let pull_request = sample_pull_request();

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": run_record.settings,
                "graph": run_record.graph,
                "workflow_source": "digraph night_sky {}",
                "workflow_slug": run_record.workflow_slug,
                "working_directory": run_record.working_directory,
                "host_repo_path": run_record.host_repo_path,
                "base_branch": run_record.base_branch,
                "labels": run_record.labels,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:05Z",
            "run.started",
            None,
            serde_json::json!({
                "run_branch": start_record.run_branch,
                "base_sha": start_record.base_sha,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:06Z",
            "run.running",
            None,
            serde_json::json!({
                "reason": status_record.reason,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:07Z",
            "checkpoint.completed",
            Some("code"),
            serde_json::json!({
                "status": "success",
                "current_node": checkpoint.current_node,
                "completed_nodes": checkpoint.completed_nodes,
                "node_retries": checkpoint.node_retries,
                "context_values": checkpoint.context_values,
                "node_outcomes": checkpoint.node_outcomes,
                "next_node_id": checkpoint.next_node_id,
                "git_commit_sha": checkpoint.git_commit_sha,
                "loop_failure_signatures": serde_json::json!({}),
                "restart_failure_signatures": serde_json::json!({}),
                "node_visits": checkpoint.node_visits,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08Z",
            "sandbox.initialized",
            None,
            serde_json::json!({
                "provider": sandbox.provider,
                "working_directory": sandbox.working_directory,
                "identifier": sandbox.identifier,
                "host_working_directory": sandbox.host_working_directory,
                "container_mount_point": sandbox.container_mount_point,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.1Z",
            "stage.prompt",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "text": "Plan the fix",
                "mode": "prompt",
                "provider": "openai",
                "model": "gpt-5.4"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.2Z",
            "command.started",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "command": "cargo test"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.3Z",
            "command.completed",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "stdout": "ok",
                "stderr": "",
                "exit_code": 0
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.4Z",
            "checkpoint.completed",
            Some("code"),
            serde_json::json!({
                "status": "success",
                "ordinal": 2,
                "current_node": checkpoint.current_node,
                "completed_nodes": checkpoint.completed_nodes,
                "node_retries": checkpoint.node_retries,
                "context_values": checkpoint.context_values,
                "node_outcomes": checkpoint.node_outcomes,
                "next_node_id": checkpoint.next_node_id,
                "git_commit_sha": checkpoint.git_commit_sha,
                "node_visits": checkpoint.node_visits,
                "diff": "diff --git a/src/lib.rs b/src/lib.rs"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.5Z",
            "parallel.completed",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "results": [{"node_id": "lint", "status": "success"}]
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.6Z",
            "stage.completed",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "status": "success",
                "notes": "all good",
                "response": "Implemented",
                "files_touched": ["src/lib.rs"]
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08.7Z",
            "retro.started",
            None,
            serde_json::json!({
                "prompt": "How did it go?"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:09Z",
            "retro.completed",
            None,
            serde_json::json!({
                "response": "Smooth enough",
                "retro": retro,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:10Z",
            "run.completed",
            None,
            serde_json::json!({
                "status": conclusion.status,
                "duration_ms": conclusion.duration_ms,
                "total_cost": conclusion.total_cost,
                "final_git_commit_sha": conclusion.final_git_commit_sha,
                "final_patch": "diff --git a/src/lib.rs b/src/lib.rs\n",
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:11Z",
            "pull_request.created",
            None,
            serde_json::json!({
                "pr_url": pull_request.html_url,
                "pr_number": pull_request.number,
                "owner": pull_request.owner,
                "repo": pull_request.repo,
                "base_branch": pull_request.base_branch,
                "head_branch": pull_request.head_branch,
                "title": pull_request.title,
            }),
        ))
        .await
        .unwrap();
        run.put_artifact_value("summary", &serde_json::json!({"done": true}))
            .await
            .unwrap();
        run.put_asset(&node, "src/lib.rs", b"fn main() {}")
            .await
            .unwrap();

        let state = run.state().await.unwrap();
        let stored_run = state.run.as_ref().unwrap();
        assert_eq!(stored_run.run_id, run_record.run_id);
        assert_eq!(stored_run.created_at, run_record.created_at);
        assert_eq!(stored_run.workflow_slug, run_record.workflow_slug);
        assert_eq!(stored_run.graph.name, run_record.graph.name);
        assert_eq!(state.graph_source.as_deref(), Some("digraph night_sky {}"));

        let stored_start = state.start.as_ref().unwrap();
        assert_eq!(stored_start.run_id, start_record.run_id);
        assert_eq!(stored_start.start_time, start_record.start_time);

        let stored_status = state.status.as_ref().unwrap();
        assert_eq!(stored_status.status, RunStatus::Succeeded);
        assert_eq!(stored_status.reason, None);

        let stored_checkpoint = state.checkpoint.as_ref().unwrap();
        assert_eq!(stored_checkpoint.current_node, checkpoint.current_node);
        assert_eq!(stored_checkpoint.next_node_id, checkpoint.next_node_id);

        let stored_conclusion = state.conclusion.as_ref().unwrap();
        assert_eq!(stored_conclusion.status, conclusion.status);
        assert_eq!(stored_conclusion.duration_ms, conclusion.duration_ms);
        assert_eq!(stored_conclusion.total_cost, conclusion.total_cost);

        let stored_retro = state.retro.as_ref().unwrap();
        assert_eq!(stored_retro.run_id, retro.run_id);
        assert_eq!(stored_retro.intent, retro.intent);
        let stored_sandbox = state.sandbox.as_ref().unwrap();
        assert_eq!(stored_sandbox.provider, sandbox.provider);
        assert_eq!(stored_sandbox.working_directory, sandbox.working_directory);
        assert_eq!(state.retro_prompt.as_deref(), Some("How did it go?"));
        assert_eq!(state.retro_response.as_deref(), Some("Smooth enough"));
        assert_eq!(
            run.get_artifact_value("summary").await.unwrap(),
            Some(serde_json::json!({"done": true}))
        );
        assert_eq!(
            run.get_asset(&node, "src/lib.rs").await.unwrap(),
            Some(Bytes::from_static(b"fn main() {}"))
        );
        assert_eq!(
            state.final_patch.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs\n")
        );
        assert_eq!(state.pull_request, Some(pull_request.clone()));
        assert!(state.iter_nodes().any(|(node, _)| node.node_id() == "code"));
        let node_state = state
            .node(&node)
            .expect("node state should exist for code:2");
        assert_eq!(node_state.prompt.as_deref(), Some("Plan the fix"));
        assert_eq!(node_state.response.as_deref(), Some("Implemented"));
        assert_eq!(node_state.stdout.as_deref(), Some("ok"));
        assert_eq!(node_state.stderr.as_deref(), Some(""));
        assert_eq!(
            node_state.diff.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs")
        );
        assert_eq!(
            node_state
                .provider_used
                .as_ref()
                .and_then(|v| v.get("provider"))
                .and_then(|v| v.as_str()),
            Some("openai")
        );
    }

    #[tokio::test]
    async fn state_projects_event_stream() {
        let (_object_store, store) = make_store();
        let created_at = dt("2026-03-27T12:00:00Z");
        let run = store
            .create_run(&test_run_id("run-1"), created_at, None)
            .await
            .unwrap();
        let run_record = sample_run_record("run-1", created_at);
        let retro = sample_retro("run-1");

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": run_record.settings,
                "graph": run_record.graph,
                "workflow_source": "digraph night_sky {}",
                "workflow_slug": run_record.workflow_slug,
                "working_directory": run_record.working_directory,
                "host_repo_path": run_record.host_repo_path,
                "base_branch": run_record.base_branch,
                "labels": run_record.labels,
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:05Z",
            "run.started",
            None,
            serde_json::json!({
                "run_branch": "fabro/run/demo",
                "base_sha": "abc123"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:06Z",
            "run.running",
            None,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:07Z",
            "stage.prompt",
            Some("code"),
            serde_json::json!({
                "visit": 2,
                "text": "Plan the fix",
                "mode": "prompt",
                "provider": "openai",
                "model": "gpt-5.4"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:08Z",
            "stage.completed",
            Some("code"),
            serde_json::json!({
                "status": "success",
                "notes": "all good",
                "response": "Implemented",
                "files_touched": ["src/lib.rs"],
                "node_visits": {"code": 2}
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:09Z",
            "checkpoint.completed",
            Some("code"),
            serde_json::json!({
                "status": "success",
                "current_node": "code",
                "completed_nodes": ["plan"],
                "context_values": {"artifact": {"kind": "summary"}},
                "next_node_id": "review",
                "git_commit_sha": "def456",
                "node_visits": {"code": 2},
                "diff": "diff --git a/src/lib.rs b/src/lib.rs"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:10Z",
            "sandbox.initialized",
            None,
            serde_json::json!({
                "provider": "local",
                "working_directory": "/tmp/night-sky",
                "identifier": "sandbox-1",
                "host_working_directory": "/tmp/night-sky"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:11Z",
            "retro.started",
            None,
            serde_json::json!({
                "prompt": "How did it go?"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:12Z",
            "retro.completed",
            None,
            serde_json::json!({
                "response": "Smooth enough",
                "retro": retro
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:13Z",
            "pull_request.created",
            None,
            serde_json::json!({
                "pr_url": "https://github.com/fabro-sh/fabro/pull/123",
                "pr_number": 123,
                "owner": "fabro-sh",
                "repo": "fabro",
                "base_branch": "main",
                "head_branch": "fabro/run/demo",
                "title": "Map the constellations",
                "draft": false
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:15Z",
            "run.completed",
            None,
            serde_json::json!({
                "duration_ms": 3210,
                "artifact_count": 1,
                "status": "success",
                "total_cost": 1.25,
                "final_git_commit_sha": "feedbeef",
                "final_patch": "diff --git a/src/lib.rs b/src/lib.rs\n"
            }),
        ))
        .await
        .unwrap();

        let state = run.state().await.unwrap();
        assert_eq!(
            state.run.as_ref().map(|run| run.run_id),
            Some(test_run_id("run-1"))
        );
        assert_eq!(state.graph_source.as_deref(), Some("digraph night_sky {}"));
        assert_eq!(
            state
                .start
                .as_ref()
                .and_then(|start| start.run_branch.as_deref()),
            Some("fabro/run/demo")
        );
        assert_eq!(
            state.status.as_ref().map(|status| status.status),
            Some(RunStatus::Succeeded)
        );
        assert_eq!(
            state
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.current_node.as_str()),
            Some("code")
        );
        assert_eq!(state.checkpoints.len(), 1);
        assert_eq!(
            state.final_patch.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs\n")
        );
        assert_eq!(state.retro_prompt.as_deref(), Some("How did it go?"));
        assert_eq!(state.retro_response.as_deref(), Some("Smooth enough"));
        assert_eq!(state.pull_request.as_ref().map(|pr| pr.number), Some(123));
        assert_eq!(
            state
                .sandbox
                .as_ref()
                .map(|sandbox| sandbox.provider.as_str()),
            Some("local")
        );
        assert_eq!(state.list_node_visits("code"), vec![2]);
        let node = state.node(&StageId::new("code", 2)).unwrap();
        assert_eq!(node.prompt.as_deref(), Some("Plan the fix"));
        assert_eq!(node.response.as_deref(), Some("Implemented"));
        assert_eq!(
            node.diff.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs")
        );
        assert_eq!(
            node.provider_used
                .as_ref()
                .and_then(|value| value.get("provider"))
                .and_then(|value| value.as_str()),
            Some("openai")
        );
    }

    #[tokio::test]
    async fn state_rewind_keeps_active_projection_only() {
        let (_object_store, store) = make_store();
        let run = store
            .create_run(&test_run_id("run-1"), dt("2026-03-27T12:00:00Z"), None)
            .await
            .unwrap();

        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:00Z",
            "run.created",
            None,
            serde_json::json!({
                "settings": Settings::default(),
                "graph": Graph::new("night-sky"),
                "working_directory": "/tmp/night-sky",
                "labels": {}
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:01Z",
            "stage.prompt",
            Some("code"),
            serde_json::json!({
                "visit": 1,
                "text": "before rewind"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:02Z",
            "pull_request.created",
            None,
            serde_json::json!({
                "pr_url": "https://github.com/fabro-sh/fabro/pull/123",
                "pr_number": 123,
                "owner": "fabro-sh",
                "repo": "fabro",
                "base_branch": "main",
                "head_branch": "fabro/run/demo",
                "title": "Map the constellations",
                "draft": false
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:03Z",
            "run.completed",
            None,
            serde_json::json!({
                "duration_ms": 10,
                "artifact_count": 0,
                "status": "success",
                "final_patch": "old patch"
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:04Z",
            "run.rewound",
            None,
            serde_json::json!({
                "target_checkpoint_ordinal": 1,
                "target_node_id": "plan",
                "target_visit": 1
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:05Z",
            "checkpoint.completed",
            Some("plan"),
            serde_json::json!({
                "status": "success",
                "current_node": "plan",
                "completed_nodes": [],
                "node_visits": {"plan": 1}
            }),
        ))
        .await
        .unwrap();
        run.append_event(&event_payload(
            "run-1",
            "2026-03-27T12:00:06Z",
            "run.submitted",
            None,
            serde_json::json!({}),
        ))
        .await
        .unwrap();

        let state = run.state().await.unwrap();
        assert_eq!(
            state.status.as_ref().map(|status| status.status),
            Some(RunStatus::Submitted)
        );
        assert!(state.conclusion.is_none());
        assert!(state.final_patch.is_none());
        assert!(state.pull_request.is_none());
        assert_eq!(state.checkpoints.len(), 1);
        assert_eq!(
            state
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.current_node.as_str()),
            Some("plan")
        );
        assert!(state.is_empty());
    }

    #[tokio::test]
    async fn append_event_validates_payload_shape_and_run_id() {
        let (_object_store, store) = make_store();
        let run = store
            .create_run(&test_run_id("run-1"), dt("2026-03-27T12:00:00Z"), None)
            .await
            .unwrap();

        let invalid_missing_err = serde_json::from_value::<EventPayload>(serde_json::json!({
            "run_id": "run-1"
        }))
        .unwrap_err();
        assert!(
            invalid_missing_err
                .to_string()
                .contains("missing or non-string required field: id")
        );

        let invalid_run_id: EventPayload = serde_json::from_value(serde_json::json!({
            "id": "evt-invalid-run",
            "ts": "2026-03-27T12:00:00Z",
            "run_id": "other-run",
            "event": "StageStarted"
        }))
        .unwrap();
        let err = run.append_event(&invalid_run_id).await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidEvent(_)));
    }

    #[tokio::test]
    async fn state_retains_checkpoint_history_by_event_sequence() {
        let (_object_store, store) = make_store();
        let run = store
            .create_run(&test_run_id("run-1"), dt("2026-03-27T12:00:00Z"), None)
            .await
            .unwrap();
        let checkpoint = sample_checkpoint();
        let seq = run
            .append_event(&event_payload(
                "run-1",
                "2026-03-27T12:00:00Z",
                "checkpoint.completed",
                Some(&checkpoint.current_node),
                serde_json::json!({
                    "status": "success",
                    "current_node": checkpoint.current_node,
                    "completed_nodes": checkpoint.completed_nodes,
                    "node_retries": checkpoint.node_retries,
                    "context_values": checkpoint.context_values,
                    "node_outcomes": checkpoint.node_outcomes,
                    "next_node_id": checkpoint.next_node_id,
                    "git_commit_sha": checkpoint.git_commit_sha,
                    "loop_failure_signatures": serde_json::json!({}),
                    "restart_failure_signatures": serde_json::json!({}),
                    "node_visits": checkpoint.node_visits,
                }),
            ))
            .await
            .unwrap();
        let state = run.state().await.unwrap();
        assert_eq!(seq, 1);
        assert_eq!(state.checkpoints.len(), 1);
        assert_eq!(state.checkpoints[0].0, 1);
        assert_eq!(state.checkpoints[0].1.current_node, checkpoint.current_node);
        assert_eq!(
            state.checkpoint.as_ref().unwrap().current_node,
            checkpoint.current_node
        );
    }

    #[tokio::test]
    async fn list_runs_filters_dates_and_tolerates_missing_status() {
        let (_object_store, store) = make_store();
        let early = dt("2026-03-27T10:00:00Z");
        let late = dt("2026-03-27T12:00:00Z");

        let early_run = store
            .create_run(&test_run_id("run-early"), early, None)
            .await
            .unwrap();
        let early_record = sample_run_record("run-early", early);
        early_run
            .append_event(&event_payload(
                "run-early",
                "2026-03-27T10:00:00Z",
                "run.created",
                None,
                serde_json::json!({
                    "settings": early_record.settings,
                    "graph": early_record.graph,
                    "workflow_slug": early_record.workflow_slug,
                    "working_directory": early_record.working_directory,
                    "host_repo_path": early_record.host_repo_path,
                    "base_branch": early_record.base_branch,
                    "labels": early_record.labels,
                }),
            ))
            .await
            .unwrap();

        let late_run = store
            .create_run(&test_run_id("run-late"), late, None)
            .await
            .unwrap();
        let late_record = sample_run_record("run-late", late);
        late_run
            .append_event(&event_payload(
                "run-late",
                "2026-03-27T12:00:00Z",
                "run.created",
                None,
                serde_json::json!({
                    "settings": late_record.settings,
                    "graph": late_record.graph,
                    "workflow_slug": late_record.workflow_slug,
                    "working_directory": late_record.working_directory,
                    "host_repo_path": late_record.host_repo_path,
                    "base_branch": late_record.base_branch,
                    "labels": late_record.labels,
                }),
            ))
            .await
            .unwrap();
        late_run
            .append_event(&event_payload(
                "run-late",
                "2026-03-27T12:00:01Z",
                "run.started",
                None,
                serde_json::json!({
                    "run_branch": "fabro/run/demo",
                    "base_sha": "abc123",
                }),
            ))
            .await
            .unwrap();
        late_run
            .append_event(&event_payload(
                "run-late",
                "2026-03-27T12:00:02Z",
                "run.completed",
                None,
                serde_json::json!({
                    "duration_ms": 3210,
                    "artifact_count": 1,
                    "status": "success",
                    "reason": "completed",
                    "total_cost": 1.25,
                }),
            ))
            .await
            .unwrap();

        let all = store.list_runs().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].catalog.run_id, test_run_id("run-late"));
        assert_eq!(all[0].workflow_name, Some("night-sky".to_string()));
        assert_eq!(all[0].goal, Some("map the constellations".to_string()));
        assert_eq!(
            all[0].host_repo_path,
            Some("github.com/fabro-sh/fabro".to_string())
        );
        assert_eq!(all[0].duration_ms, Some(3210));
        assert_eq!(all[0].total_cost, Some(1.25));
        assert_eq!(all[0].status_reason, Some(StatusReason::Completed));
        assert_eq!(all[1].status, None);

        let filtered = store
            .list_runs_in_range(
                Some(dt("2026-03-27T11:00:00Z")),
                Some(dt("2026-03-27T13:00:00Z")),
            )
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].catalog.run_id, test_run_id("run-late"));
    }
}
