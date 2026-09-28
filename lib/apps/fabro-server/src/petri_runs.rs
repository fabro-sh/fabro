//! The Petri runs the server holds open for its workers.
//!
//! A worker reaches its run's Petri records over the API
//! (`/api/v1/runs/{id}/petri/*`, `server::handler::petri`), and the server
//! answers from one `SqliteRunStore` over its pool. The store's lease is
//! held by a handle, so the server keeps the handle a worker opened, keyed
//! by the run and the worker's owner id, for as long as the worker's lease
//! should last: until the worker releases it, or until the server observes
//! the worker exit. That is the integration plan's rule for a lease: it ends
//! when the handle drops, when the server observes the worker exit, or by
//! operator release, never by timeout.
//!
//! The Petri run key of a Fabro run is the run id's text, as the plan sets
//! `RunOptions::run_key`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fabro_db::DbPool;
use fabro_petri::SqliteRunStore;
use fabro_petri::petri::{Access, OwnerId, RunKey, RunLogs, RunStore as _, StoreError};
use fabro_types::RunId;
use fabro_util::sync;
use tracing::debug;

pub(crate) struct PetriRuns {
    store:   Arc<SqliteRunStore>,
    /// The writer handle each worker holds open, by run and owner.
    handles: Mutex<HashMap<(RunId, OwnerId), Arc<dyn RunLogs>>>,
}

impl PetriRuns {
    pub(crate) fn new(pool: DbPool) -> Self {
        Self {
            store:   Arc::new(SqliteRunStore::new(pool)),
            handles: Mutex::default(),
        }
    }

    /// The store the workers' handles are open on, for the server's own
    /// work on a run's record (the sandbox prune at deletion). The same
    /// instance matters: a handle dropped here has its lease release
    /// awaited by this store's next open, so a prune right after
    /// [`worker_exited`](Self::worker_exited) finds the lease free.
    pub(crate) fn shared_store(&self) -> Arc<SqliteRunStore> {
        Arc::clone(&self.store)
    }

    /// The Petri run key of a Fabro run.
    pub(crate) fn key(run_id: &RunId) -> RunKey {
        RunKey::new(run_id.to_string())
    }

    /// The store itself, for an operator's release and for inspection.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn store(&self) -> &SqliteRunStore {
        &self.store
    }

    /// Open the run as a worker asked. A writer handle is kept for the
    /// owner until [`release`](Self::release) or
    /// [`worker_exited`](Self::worker_exited); a reader handle is not kept.
    pub(crate) async fn open(
        &self,
        run_id: RunId,
        access: Access,
    ) -> Result<Arc<dyn RunLogs>, StoreError> {
        let handle = self.store.open(&Self::key(&run_id), access.clone()).await?;
        if let Some(owner) = access.owner() {
            sync::lock(&self.handles).insert((run_id, owner.clone()), Arc::clone(&handle));
        }
        Ok(handle)
    }

    /// The writer handle `owner` holds on the run: the one kept from its
    /// open, or a reopen when the store's lease row still names the owner
    /// (the server restarted, or the open's reply was lost). An owner the
    /// lease no longer names gets `StaleOwner`.
    pub(crate) async fn writer(
        &self,
        run_id: RunId,
        owner: &OwnerId,
    ) -> Result<Arc<dyn RunLogs>, StoreError> {
        if let Some(handle) = sync::lock(&self.handles).get(&(run_id, owner.clone())) {
            return Ok(Arc::clone(handle));
        }
        let holder = self.store.owner(&Self::key(&run_id)).await?;
        if holder.as_ref() != Some(owner) {
            return Err(StoreError::StaleOwner);
        }
        debug!(run_id = %run_id, owner = %owner, "Petri run handle reopened for its lease holder");
        match self
            .open(run_id, Access::Write {
                owner: owner.clone(),
            })
            .await
        {
            Err(StoreError::Leased { .. }) => Err(StoreError::StaleOwner),
            opened => opened,
        }
    }

    /// A reader handle on the run: no lease, never kept.
    pub(crate) async fn reader(&self, run_id: RunId) -> Result<Arc<dyn RunLogs>, StoreError> {
        self.store.open(&Self::key(&run_id), Access::Read).await
    }

    /// Drop the handle `owner` holds on the run: the worker's own release.
    /// The store ends the lease when this was the owner's last handle.
    pub(crate) fn release(&self, run_id: RunId, owner: &OwnerId) {
        let handle = sync::lock(&self.handles).remove(&(run_id, owner.clone()));
        debug!(
            run_id = %run_id,
            owner = %owner,
            held = handle.is_some(),
            "Petri run handle released by its worker"
        );
        drop(handle);
    }

    /// End whatever lease the run's previous worker held, from outside:
    /// what the server does for a run it finds in flight at startup, before
    /// it launches a new worker for it. The previous worker, should it still
    /// be alive, finds its handles stale on its next write. `NotFound` when
    /// the store never held the run.
    pub(crate) async fn release_for_restart(&self, run_id: RunId) -> Result<(), StoreError> {
        self.worker_exited(run_id);
        self.store.release_lease(&Self::key(&run_id)).await
    }

    /// Drop every handle held on the run: what the server does when it
    /// observes the run's worker exit, so a worker that died without
    /// releasing does not keep the lease.
    pub(crate) fn worker_exited(&self, run_id: RunId) {
        let dropped = {
            let mut handles = sync::lock(&self.handles);
            let owners: Vec<_> = handles
                .keys()
                .filter(|(held, _)| *held == run_id)
                .cloned()
                .collect();
            owners
                .into_iter()
                .filter_map(|slot| handles.remove(&slot))
                .collect::<Vec<_>>()
        };
        if !dropped.is_empty() {
            debug!(
                run_id = %run_id,
                handles = dropped.len(),
                "Petri run handles released at worker exit"
            );
        }
        drop(dropped);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use fabro_config::Storage;
    use fabro_config::bind::Bind;
    use fabro_config::daemon::ServerDaemon;
    use fabro_petri::petri::RunStore as _;
    use fabro_static::EnvVars;
    use fabro_store::platform_records::{PlatformRecord, RunLifecycleKind, RunLifecycleRecord};
    use fabro_types::{
        FailureReason, RunId, RunStatus, SuccessReason, WorkflowPath, WorkflowVersion,
    };
    use fabro_util::exit::ExitClass;
    use serde_json::json;
    use tokio::io::AsyncRead;
    use tokio::sync::Notify;
    use tokio::time;
    use tower::ServiceExt as _;

    use super::*;
    use crate::server::petri_runs::MAX_STORE_INTERRUPTIONS;
    use crate::server::{
        AppState, reconcile_incomplete_runs_on_startup, run_records, spawn_scheduler,
    };
    use crate::test_support::{
        TestAppStateBuilder, build_test_router, test_register_workflow_version,
        test_secret_store_path, test_store_bundle,
    };
    use crate::worker_runtime::{
        StartedWorker, WorkerExit, WorkerLaunchSpec, WorkerRef, WorkerRuntime,
    };

    const MINIMAL_DOT: &str = r#"digraph Test {
    graph [goal="Test"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    start -> exit
}"#;

    const PETRI_SETTINGS: &str = "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n";

    /// A worker runtime whose one worker runs until the test ends it, so
    /// the test can act while the server waits on the worker. It keeps the
    /// mode the server launched the worker with.
    #[derive(Default)]
    struct HeldWorkerRuntime {
        started: Notify,
        running: AtomicBool,
        exit:    Arc<Notify>,
        /// The exit code the worker ends with; `None` for a signal.
        code:    Arc<Mutex<Option<i32>>>,
        mode:    Mutex<Option<&'static str>>,
    }

    impl HeldWorkerRuntime {
        async fn wait_for_start(&self) {
            time::timeout(Duration::from_secs(10), self.started.notified())
                .await
                .expect("the scheduler starts the worker");
        }

        fn end_worker(&self) {
            self.end_worker_with(None);
        }

        fn end_worker_with(&self, code: Option<i32>) {
            *sync::lock(&self.code) = code;
            self.running.store(false, Ordering::SeqCst);
            self.exit.notify_one();
        }

        fn launched_mode(&self) -> Option<&'static str> {
            *sync::lock(&self.mode)
        }
    }

    #[async_trait::async_trait]
    impl WorkerRuntime for HeldWorkerRuntime {
        async fn start(&self, spec: WorkerLaunchSpec) -> anyhow::Result<StartedWorker> {
            *sync::lock(&self.mode) = Some(spec.mode);
            self.running.store(true, Ordering::SeqCst);
            let exit = Arc::clone(&self.exit);
            let code = Arc::clone(&self.code);
            let stderr: Pin<Box<dyn AsyncRead + Send + 'static>> = Box::pin(tokio::io::empty());
            let started = StartedWorker {
                worker_ref: WorkerRef::Local { pid: u32::MAX },
                stderr,
                wait: Box::pin(async move {
                    exit.notified().await;
                    let code = *sync::lock(&code);
                    Ok(WorkerExit {
                        success: code == Some(0),
                        code,
                        detail: "test worker ended without a terminal event".to_string(),
                    })
                }),
            };
            self.started.notify_one();
            Ok(started)
        }

        async fn request_stop(&self, _worker_ref: &WorkerRef) {
            self.end_worker();
        }

        async fn force_stop(&self, _worker_ref: &WorkerRef) {
            self.end_worker();
        }

        async fn is_alive(&self, _worker_ref: &WorkerRef) -> bool {
            self.running.load(Ordering::SeqCst)
        }
    }

    /// The server record the worker launch spec reads.
    fn write_test_server_record(state: &AppState) {
        let runtime_directory = Storage::new(state.server_storage_dir()).runtime_directory();
        ServerDaemon::new(
            std::process::id(),
            Bind::Tcp(
                "127.0.0.1:32276"
                    .parse()
                    .expect("the test bind address parses"),
            ),
            runtime_directory.log_path(),
        )
        .write(&runtime_directory)
        .expect("the test server record writes");
    }

    /// A legacy run created and started through the API, as a client would.
    async fn create_and_start_run(app: &axum::Router) -> RunId {
        create_and_start_run_with(app, &[]).await
    }

    /// A Petri run: its version's `workflow.toml` names the engine.
    async fn create_and_start_petri_run(app: &axum::Router) -> RunId {
        create_and_start_run_with(app, &[("workflow.toml", PETRI_SETTINGS)]).await
    }

    async fn create_and_start_run_with(app: &axum::Router, extra: &[(&str, &str)]) -> RunId {
        let path = WorkflowPath::new("workflow.fabro").expect("a workflow path");
        let mut files = BTreeMap::from([(path.clone(), MINIMAL_DOT.to_string())]);
        for (name, text) in extra {
            files.insert(
                WorkflowPath::new(*name).expect("a workflow path"),
                (*text).to_string(),
            );
        }
        let version =
            WorkflowVersion::new(path, files, BTreeMap::new()).expect("a workflow version");
        let version_id = test_register_workflow_version(app, &version, None).await;
        let intent = json!({
            "workflow_version_id": version_id,
            "target": { "kind": "none" },
            "args": {},
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/runs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(intent.to_string()))
                    .expect("the create request builds"),
            )
            .await
            .expect("the create request completes");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("the create body reads");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("the create body is JSON");
        let run_id: RunId = body["id"]
            .as_str()
            .expect("the created run has an id")
            .parse()
            .expect("the run id parses");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/runs/{run_id}/start"))
                    .body(Body::empty())
                    .expect("the start request builds"),
            )
            .await
            .expect("the start request completes");
        assert_eq!(response.status(), StatusCode::OK);
        run_id
    }

    /// The lease a worker took over the API ends when the server observes
    /// the worker exit, with no release from the worker itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_lease_ends_when_the_server_observes_the_worker_exit() {
        let runtime = Arc::new(HeldWorkerRuntime::default());
        let state = TestAppStateBuilder::new()
            .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
            .worker_runtime(Arc::clone(&runtime) as Arc<dyn WorkerRuntime>)
            .build();
        write_test_server_record(&state);
        let app = build_test_router(Arc::clone(&state));
        let run_id = create_and_start_run(&app).await;
        spawn_scheduler(Arc::clone(&state));
        runtime.wait_for_start().await;

        // The worker opens its run over the API and never releases it.
        let token = state.test_issue_worker_token(&run_id);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/runs/{run_id}/petri/open"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "access": "create", "owner": "worker-1" }).to_string(),
                    ))
                    .expect("the open request builds"),
            )
            .await
            .expect("the open request completes");
        assert_eq!(response.status(), StatusCode::OK);
        let key = PetriRuns::key(&run_id);
        let store = state.petri_runs.store();
        assert_eq!(
            store.owner(&key).await.expect("reads the lease"),
            Some(OwnerId::new("worker-1"))
        );

        runtime.end_worker();

        let mut holder = store.owner(&key).await.expect("reads the lease");
        for _ in 0..500 {
            if holder.is_none() {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
            holder = store.owner(&key).await.expect("reads the lease");
        }
        assert_eq!(holder, None, "the lease ended when the worker exited");
        let resumed = store
            .open(&key, Access::Write {
                owner: OwnerId::new("resumer"),
            })
            .await
            .expect("the next owner takes the run");
        drop(resumed);
    }

    /// After a restart, a Petri run the previous server left running goes
    /// back to a worker in resume mode: the lease its worker held is
    /// released from outside, the run is asked to start again as a resume,
    /// and the scheduler launches the worker with `--mode resume`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_petri_run_left_running_by_a_restart_goes_back_to_a_worker_in_resume_mode() {
        let (store, artifact_store) = test_store_bundle();
        let vault_path = test_secret_store_path();
        let before = TestAppStateBuilder::new()
            .store_bundle(Arc::clone(&store), artifact_store.clone())
            .vault_path(vault_path.clone())
            .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
            .build();
        let app = build_test_router(Arc::clone(&before));
        let run_id = create_and_start_petri_run(&app).await;
        let key = PetriRuns::key(&run_id);

        // The worker took the run as far as running and holds its lease;
        // then the server died, so nothing released it.
        for (transition, status) in [
            (RunLifecycleKind::Starting, RunStatus::Starting),
            (RunLifecycleKind::Running, RunStatus::Running),
        ] {
            run_records::lifecycle(
                &before,
                run_id,
                RunLifecycleRecord::new(transition).with_status(status),
            )
            .await
            .expect("the lifecycle record appends");
        }
        let held = before
            .petri_runs
            .open(run_id, Access::Create {
                owner: OwnerId::new("worker-1"),
            })
            .await
            .expect("the worker takes the run");
        drop(held);
        assert_eq!(
            before
                .petri_runs
                .store()
                .owner(&key)
                .await
                .expect("reads the lease"),
            Some(OwnerId::new("worker-1"))
        );

        let runtime = Arc::new(HeldWorkerRuntime::default());
        let after = TestAppStateBuilder::new()
            .store_bundle(store, artifact_store)
            .vault_path(vault_path)
            .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
            .worker_runtime(Arc::clone(&runtime) as Arc<dyn WorkerRuntime>)
            .build();
        let reconciled = reconcile_incomplete_runs_on_startup(&after)
            .await
            .expect("the restart reconciles");
        assert_eq!(reconciled, 1);

        assert_eq!(
            after
                .petri_runs
                .store()
                .owner(&key)
                .await
                .expect("reads the lease"),
            None,
            "the previous worker's lease is released"
        );
        let run_state = run_records::projection(&after, run_id)
            .await
            .expect("the run state loads")
            .expect("the run projects");
        assert_eq!(run_state.status, RunStatus::Runnable);
        let transitions = after
            .stores
            .run_summaries
            .platform_records()
            .read(&run_id)
            .await
            .expect("the records list")
            .into_iter()
            .filter_map(|stored| match stored.record {
                PlatformRecord::RunLifecycle(record) => Some(record.transition),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            &transitions[transitions.len() - 4..],
            [
                RunLifecycleKind::Starting,
                RunLifecycleKind::Running,
                RunLifecycleKind::StartRequested,
                RunLifecycleKind::Runnable
            ],
            "{transitions:?}"
        );

        write_test_server_record(&after);
        spawn_scheduler(Arc::clone(&after));
        runtime.wait_for_start().await;
        assert_eq!(runtime.launched_mode(), Some("resume"));
        runtime.end_worker();
        // The first server's handles must outlive the check above: a real
        // crash releases nothing, and dropping them here would.
        drop(before);
    }

    /// A server whose one worker is held open by the test, with a run the
    /// worker has taken over the API: the run's id and the worker's token.
    async fn held_worker_run(
        runtime: &Arc<HeldWorkerRuntime>,
    ) -> (Arc<AppState>, axum::Router, RunId, String) {
        let state = TestAppStateBuilder::new()
            .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
            .worker_runtime(Arc::clone(runtime) as Arc<dyn WorkerRuntime>)
            .build();
        write_test_server_record(&state);
        let app = build_test_router(Arc::clone(&state));
        let run_id = create_and_start_petri_run(&app).await;
        spawn_scheduler(Arc::clone(&state));
        runtime.wait_for_start().await;
        let token = state.test_issue_worker_token(&run_id);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/runs/{run_id}/petri/open"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "access": "create", "owner": "worker-1" }).to_string(),
                    ))
                    .expect("the open request builds"),
            )
            .await
            .expect("the open request completes");
        assert_eq!(response.status(), StatusCode::OK);
        (state, app, run_id, token)
    }

    /// One lifecycle record of the run, stored the way its worker stores
    /// one: through the platform-records endpoint.
    async fn record_lifecycle_as_worker(
        app: &axum::Router,
        run_id: RunId,
        token: &str,
        transition: RunLifecycleKind,
        status: RunStatus,
    ) {
        let record =
            PlatformRecord::RunLifecycle(RunLifecycleRecord::new(transition).with_status(status));
        let body = json!({
            "record": serde_json::to_value(&record).expect("the record encodes"),
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/runs/{run_id}/petri/platform-records"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("the append request builds"),
            )
            .await
            .expect("the append request completes");
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// Petri's own finish of the run, stored the way its worker stores it:
    /// the `run.finished` record on the coordinator log, over the records
    /// endpoint.
    async fn record_finish_as_worker(app: &axum::Router, run_id: RunId, token: &str) {
        let body = json!({
            "owner": "worker-1",
            "records": [{
                "seq": 0,
                "recorded_at": 1_000,
                "record": {
                    "seq": 0,
                    "origin": "external",
                    "recorded_at": 1_000,
                    "body": { "event": "run.finished", "status": "success" },
                },
            }],
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/v1/runs/{run_id}/petri/logs/coordinator/records"
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("the records request builds"),
            )
            .await
            .expect("the records request completes");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    /// The worker takes the run as far as running.
    async fn run_to_running_as_worker(app: &axum::Router, run_id: RunId, token: &str) {
        for (transition, status) in [
            (RunLifecycleKind::Starting, RunStatus::Starting),
            (RunLifecycleKind::Running, RunStatus::Running),
        ] {
            record_lifecycle_as_worker(app, run_id, token, transition, status).await;
        }
    }

    async fn status_of(app: &axum::Router, run_id: RunId) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/runs/{run_id}"))
                    .body(Body::empty())
                    .expect("the run request builds"),
            )
            .await
            .expect("the run request completes")
            .status()
    }

    /// The runs the server's usage aggregate has counted: it counts a run
    /// at its worker's exit, once the exit is fully handled.
    async fn concluded_runs(app: &axum::Router) -> i64 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/usage")
                    .body(Body::empty())
                    .expect("the usage request builds"),
            )
            .await
            .expect("the usage request completes");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("the usage body reads");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("the usage body is JSON");
        body["totals"]["runs"].as_i64().expect("the run count")
    }

    /// A delete of the run, as a client issues one.
    async fn delete_run(app: &axum::Router, run_id: RunId) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/runs/{run_id}"))
                    .body(Body::empty())
                    .expect("the delete request builds"),
            )
            .await
            .expect("the delete request completes")
            .status()
    }

    /// The server settles a worker's run at Petri's own finish, the record
    /// the view ends the run on, not at the worker's exit: a delete issued
    /// the moment the finish lands, while the worker still tears down, is
    /// accepted, and the worker's exit after it brings nothing back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_delete_right_after_petris_finish_is_accepted() {
        let runtime = Arc::new(HeldWorkerRuntime::default());
        let (state, app, run_id, token) = held_worker_run(&runtime).await;

        run_to_running_as_worker(&app, run_id, &token).await;
        record_finish_as_worker(&app, run_id, &token).await;
        assert_eq!(
            state.test_managed_run_status(&run_id),
            Some(RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            }),
            "the managed run settled at the finish"
        );
        assert!(
            runtime.running.load(Ordering::SeqCst),
            "the worker is still up when the delete lands"
        );
        assert_eq!(delete_run(&app, run_id).await, StatusCode::NO_CONTENT);

        // The delete ended the worker; its exit is handled in the
        // background and must leave the run gone.
        assert!(!runtime.running.load(Ordering::SeqCst));
        for _ in 0..20 {
            assert_eq!(status_of(&app, run_id).await, StatusCode::NOT_FOUND);
            assert_eq!(state.test_managed_run_status(&run_id), None);
            time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A worker that ends the run without Petri's finish (it failed before
    /// the engine ran) settles it at its terminal lifecycle record, so a
    /// delete issued the moment that record lands is accepted too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_delete_right_after_the_workers_terminal_record_is_accepted() {
        let runtime = Arc::new(HeldWorkerRuntime::default());
        let (state, app, run_id, token) = held_worker_run(&runtime).await;

        let failed = RunStatus::Failed {
            reason: FailureReason::WorkflowError,
        };
        record_lifecycle_as_worker(
            &app,
            run_id,
            &token,
            RunLifecycleKind::Starting,
            RunStatus::Starting,
        )
        .await;
        record_lifecycle_as_worker(&app, run_id, &token, RunLifecycleKind::Failed, failed).await;
        assert_eq!(
            state.test_managed_run_status(&run_id),
            Some(failed),
            "the managed run settled at the terminal record"
        );
        assert!(
            runtime.running.load(Ordering::SeqCst),
            "the worker is still up when the delete lands"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/runs/{run_id}"))
                    .body(Body::empty())
                    .expect("the delete request builds"),
            )
            .await
            .expect("the delete request completes");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // The delete ended the worker; its exit is handled in the
        // background and must leave the run gone.
        assert!(!runtime.running.load(Ordering::SeqCst));
        for _ in 0..20 {
            assert_eq!(status_of(&app, run_id).await, StatusCode::NOT_FOUND);
            assert_eq!(state.test_managed_run_status(&run_id), None);
            time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A worker that exits, even unsuccessfully, after its run settled at
    /// its finish and its terminal record leaves the settled status alone:
    /// the exit reaps the process and counts the run, nothing more.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_worker_exit_after_the_run_settled_keeps_the_settled_status() {
        let runtime = Arc::new(HeldWorkerRuntime::default());
        let (state, app, run_id, token) = held_worker_run(&runtime).await;
        let succeeded = RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        };

        run_to_running_as_worker(&app, run_id, &token).await;
        record_finish_as_worker(&app, run_id, &token).await;
        record_lifecycle_as_worker(&app, run_id, &token, RunLifecycleKind::Succeeded, succeeded)
            .await;
        assert_eq!(state.test_managed_run_status(&run_id), Some(succeeded));

        // The held worker exits unsuccessfully, as a worker that crashed
        // after its terminal record would.
        runtime.end_worker();
        let mut counted = concluded_runs(&app).await;
        for _ in 0..500 {
            if counted == 1 {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
            counted = concluded_runs(&app).await;
        }
        assert_eq!(counted, 1, "the worker's exit was handled");

        assert_eq!(
            state.test_managed_run_status(&run_id),
            Some(succeeded),
            "the exit did not move the settled status"
        );
        let run_state = run_records::projection(&state, run_id)
            .await
            .expect("the run state loads")
            .expect("the run projects");
        assert_eq!(run_state.status, succeeded);
    }

    /// Wait until the run's stored status is terminal, and return it with
    /// the failure's message.
    async fn terminal_status(state: &Arc<AppState>, run_id: RunId) -> (RunStatus, Option<String>) {
        for _ in 0..1000 {
            let run_state = run_records::projection(state, run_id)
                .await
                .expect("the run state loads")
                .expect("the run projects");
            if run_state.status.is_terminal() {
                let message = run_state
                    .conclusion
                    .as_ref()
                    .and_then(|conclusion| conclusion.failure.as_ref())
                    .map(|failure| failure.detail.message.clone());
                return (run_state.status, message);
            }
            time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the run never ended");
    }

    /// A worker whose run's store failed exits with `EX_TEMPFAIL`: the run
    /// is not over, and goes back to a worker in resume mode, as after a
    /// crash. The previous worker's lease is released first.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_its_store_interrupted_goes_back_to_a_worker_in_resume_mode() {
        let runtime = Arc::new(HeldWorkerRuntime::default());
        let (state, app, run_id, token) = held_worker_run(&runtime).await;
        run_to_running_as_worker(&app, run_id, &token).await;
        assert_eq!(runtime.launched_mode(), Some("start"));

        runtime.end_worker_with(Some(ExitClass::Interrupted.code()));
        runtime.wait_for_start().await;

        assert_eq!(runtime.launched_mode(), Some("resume"));
        assert_eq!(
            state
                .petri_runs
                .store()
                .owner(&PetriRuns::key(&run_id))
                .await
                .expect("reads the lease"),
            None,
            "the interrupted worker's lease is released"
        );
        let transitions = state
            .stores
            .run_summaries
            .platform_records()
            .read(&run_id)
            .await
            .expect("the records list")
            .into_iter()
            .filter_map(|stored| match stored.record {
                PlatformRecord::RunLifecycle(record) => Some(record.transition),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            &transitions[transitions.len() - 4..],
            [
                RunLifecycleKind::Starting,
                RunLifecycleKind::Running,
                RunLifecycleKind::StartRequested,
                RunLifecycleKind::Runnable
            ],
            "the run was asked to start again as a resume, and did not end: {transitions:?}"
        );
        runtime.end_worker();
    }

    /// A store that keeps failing is not a glitch a resume gets past: the
    /// run resumes after each of its first interruptions, and the next one
    /// fails it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_its_store_keeps_interrupting_fails() {
        let runtime = Arc::new(HeldWorkerRuntime::default());
        let (state, app, run_id, token) = held_worker_run(&runtime).await;
        run_to_running_as_worker(&app, run_id, &token).await;

        for _ in 0..MAX_STORE_INTERRUPTIONS {
            runtime.end_worker_with(Some(ExitClass::Interrupted.code()));
            runtime.wait_for_start().await;
            assert_eq!(runtime.launched_mode(), Some("resume"));
        }
        runtime.end_worker_with(Some(ExitClass::Interrupted.code()));

        let (status, message) = terminal_status(&state, run_id).await;
        assert_eq!(status, RunStatus::Failed {
            reason: FailureReason::Terminated,
        });
        let message = message.expect("the failure has a message");
        assert!(
            message.contains(&format!(
                "The run's store failed {} times",
                MAX_STORE_INTERRUPTIONS + 1
            )),
            "{message}"
        );
    }

    /// Any other failed worker exit fails the run, as before.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_worker_that_fails_otherwise_fails_the_run() {
        let runtime = Arc::new(HeldWorkerRuntime::default());
        let (state, app, run_id, token) = held_worker_run(&runtime).await;
        run_to_running_as_worker(&app, run_id, &token).await;

        runtime.end_worker_with(Some(1));

        let (status, message) = terminal_status(&state, run_id).await;
        assert_eq!(status, RunStatus::Failed {
            reason: FailureReason::Terminated,
        });
        assert!(
            message.is_some_and(|message| message.contains("Worker exited before emitting")),
            "the failure names the worker's exit"
        );
    }
}
