//! The Petri run store over HTTP: Petri's store conformance suite against
//! `HttpRunStore` talking to a loopback server, the operator release through
//! the server's store, a lost reply to an append, and two workers contending
//! for one run's lease.

use std::collections::HashMap;
use std::future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use fabro_client::{Client, Credential, apply_bearer_token_auth};
use fabro_petri::HttpRunStore;
use fabro_petri::petri::{Access, LogId, OwnerId, RunKey, RunLogs, RunStore, StoreError};
use fabro_petri::test_support::run_store::{self, conformance, stale_owner_conformance};
use fabro_server::server::{AppState, RouterOptions, build_router_with_options};
use fabro_server::test_support::{test_app_state, test_auth_mode};
use fabro_types::RunId;
use tokio::net::TcpListener;
use tokio::runtime::Handle;
use tokio::task;

/// A loopback server over `state`, its router wrapped by `wrap`.
async fn serve(state: Arc<AppState>, wrap: impl FnOnce(Router) -> Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback listener binds");
    let addr = listener.local_addr().expect("the listener has an address");
    let router = wrap(build_router_with_options(
        state,
        &test_auth_mode(),
        RouterOptions::default(),
    ));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

/// A worker's client: the run's worker token as its bearer, and a request
/// timeout after which a reply counts as lost.
async fn worker_client(base_url: &str, token: &str, timeout: Duration) -> Client {
    let http = apply_bearer_token_auth(fabro_http::HttpClientBuilder::new().no_proxy(), token)
        .expect("the bearer header builds")
        .build()
        .expect("the test HTTP client builds");
    Client::builder()
        .transport(base_url, http)
        .credential(Credential::Worker(token.to_string()))
        .request_timeout(timeout)
        .connect()
        .await
        .expect("the worker client connects")
}

/// The Petri key of a Fabro run: its id.
fn petri_key(run_id: RunId) -> RunKey {
    RunKey::new(run_id.to_string())
}

/// Wait until the server's store shows no lease holder on `key`: a dropped
/// handle's release travels to the server on its own, and only the store
/// that dropped it awaits that. Another worker starts after the first is
/// gone, which is what this waits for.
async fn wait_until_released(store: &fabro_petri::SqliteRunStore, key: &RunKey) {
    for _ in 0..500 {
        if store.owner(key).await.expect("reads the lease").is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the lease on {key} was not released");
}

/// One worker's view of one run: a store over a client whose token names
/// the run.
async fn worker_store(state: &AppState, base_url: &str, run_id: RunId) -> HttpRunStore {
    let token = state.test_issue_worker_token(&run_id);
    HttpRunStore::new(worker_client(base_url, &token, Duration::from_secs(5)).await)
}

/// The suite opens runs under keys of its own (`lifecycle`, `drop`), while a
/// key over the API is a Fabro run id that the worker's token names. This
/// adapter gives each suite key a Fabro run of its own, with a token minted
/// for that run alone: the least a worker holds.
struct WorkerRuns {
    state:    Arc<AppState>,
    base_url: String,
    runs:     Mutex<HashMap<RunKey, Arc<WorkerRun>>>,
}

struct WorkerRun {
    run_id: RunId,
    store:  HttpRunStore,
}

impl WorkerRuns {
    fn new(state: Arc<AppState>, base_url: String) -> Self {
        Self {
            state,
            base_url,
            runs: Mutex::default(),
        }
    }

    async fn run(&self, key: &RunKey) -> Arc<WorkerRun> {
        if let Some(run) = self.runs.lock().expect("runs lock").get(key) {
            return Arc::clone(run);
        }
        let run_id = RunId::new();
        let store = worker_store(&self.state, &self.base_url, run_id).await;
        let run = Arc::new(WorkerRun { run_id, store });
        self.runs
            .lock()
            .expect("runs lock")
            .insert(key.clone(), Arc::clone(&run));
        run
    }

    /// The Fabro run a suite key was given, once opened.
    fn run_id(&self, key: &RunKey) -> RunId {
        self.runs
            .lock()
            .expect("runs lock")
            .get(key)
            .expect("the suite opens a key before it releases it")
            .run_id
    }
}

#[async_trait::async_trait]
impl RunStore for WorkerRuns {
    async fn open(&self, key: &RunKey, access: Access) -> Result<Arc<dyn RunLogs>, StoreError> {
        let run = self.run(key).await;
        run.store.open(&petri_key(run.run_id), access).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_http_store_passes_the_conformance_suite() {
    let state = test_app_state();
    let base_url = serve(Arc::clone(&state), |router| router).await;
    let runs: Arc<dyn RunStore> = Arc::new(WorkerRuns::new(state, base_url));
    conformance(|| Arc::clone(&runs)).await;
}

/// An operator release through the server's own store ends the worker's
/// lease from outside: the worker's handle turns stale and the next writer
/// takes the run. The release is async, so the suite's synchronous closure
/// blocks on it in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_release_through_the_server_makes_the_worker_stale() {
    let state = test_app_state();
    let base_url = serve(Arc::clone(&state), |router| router).await;
    let runs = WorkerRuns::new(Arc::clone(&state), base_url);
    let release = |key: &RunKey| {
        let key = petri_key(runs.run_id(key));
        let store = state.test_petri_run_store();
        task::block_in_place(|| Handle::current().block_on(store.release_lease(&key)))
            .expect("the operator releases the lease");
    };
    stale_owner_conformance(&runs, release).await;
}

/// Swallow the reply of the first append the server commits: the handler
/// runs, its transaction commits, and the response never leaves. That is
/// a lost reply as the worker sees it.
async fn swallow_one_append_reply(
    State(swallowed): State<Arc<AtomicUsize>>,
    request: Request,
    next: Next,
) -> Response {
    let is_append = request.method() == Method::POST && request.uri().path().ends_with("/records");
    let response = next.run(request).await;
    if is_append
        && response.status() == StatusCode::NO_CONTENT
        && swallowed
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        future::pending::<()>().await;
    }
    response
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_reply_to_an_append_is_safe_to_retry() {
    let state = test_app_state();
    let swallowed = Arc::new(AtomicUsize::new(0));
    let base_url = serve(Arc::clone(&state), {
        let swallowed = Arc::clone(&swallowed);
        move |router| {
            router.layer(middleware::from_fn_with_state(
                swallowed,
                swallow_one_append_reply,
            ))
        }
    })
    .await;
    let run_id = RunId::new();
    let token = state.test_issue_worker_token(&run_id);
    let timeout = Duration::from_millis(500);
    let store = HttpRunStore::new(worker_client(&base_url, &token, timeout).await);
    let key = petri_key(run_id);
    let logs = store
        .open(&key, Access::Create {
            owner: OwnerId::new("worker"),
        })
        .await
        .expect("the worker creates the run");

    let batch = [
        run_store::record(0, "execution.started"),
        run_store::record(1, "step.started"),
    ];
    let started = Instant::now();
    logs.append(&LogId::Coordinator, &batch)
        .await
        .expect("the append succeeds once the resend is answered");
    assert_eq!(
        swallowed.load(Ordering::SeqCst),
        1,
        "the server committed one append whose reply was swallowed"
    );
    assert!(
        started.elapsed() >= timeout,
        "the first attempt waited out the request timeout"
    );
    assert_eq!(
        logs.read(&LogId::Coordinator).await.expect("reads"),
        batch.to_vec(),
        "the log holds each record once"
    );
}

/// Two workers with owners of their own never hold one run's lease at the
/// same time, in either order, and the server's store reports who holds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_workers_cannot_both_hold_a_run_lease() {
    let state = test_app_state();
    let base_url = serve(Arc::clone(&state), |router| router).await;
    let run_id = RunId::new();
    let key = petri_key(run_id);
    let first_worker = worker_store(&state, &base_url, run_id).await;
    let second_worker = worker_store(&state, &base_url, run_id).await;
    let first = OwnerId::new("first");
    let second = OwnerId::new("second");
    let server_store = state.test_petri_run_store();

    let held = first_worker
        .open(&key, Access::Create {
            owner: first.clone(),
        })
        .await
        .expect("the first worker creates");
    assert_eq!(
        server_store.owner(&key).await.expect("reads"),
        Some(first.clone())
    );
    let refused = second_worker
        .open(&key, Access::Write {
            owner: second.clone(),
        })
        .await
        .err()
        .expect("the second worker is refused while the first holds the lease");
    assert!(
        matches!(&refused, StoreError::Leased { owner, .. } if *owner == first),
        "{refused}"
    );
    assert!(
        refused.to_string().contains(&run_id.to_string()),
        "the message names the run: {refused}"
    );

    drop(held);
    wait_until_released(server_store, &key).await;
    let taken = second_worker
        .open(&key, Access::Write {
            owner: second.clone(),
        })
        .await
        .expect("the second worker takes the run once the first releases");
    assert_eq!(
        server_store.owner(&key).await.expect("reads"),
        Some(second.clone())
    );
    let refused = first_worker
        .open(&key, Access::Write {
            owner: first.clone(),
        })
        .await
        .err()
        .expect("the first worker is refused in turn");
    assert!(
        matches!(&refused, StoreError::Leased { owner, .. } if *owner == second),
        "{refused}"
    );

    drop(taken);
    wait_until_released(server_store, &key).await;
}

/// A worker's store leases for the worker's launch id, whatever owner Petri
/// minted for the run runtime that opened the run: the server's lease row
/// names the launch, a reopen for the same launch shares the lease, and the
/// launch's release ends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_store_leases_for_its_launch_not_for_petris_owner() {
    let state = test_app_state();
    let base_url = serve(Arc::clone(&state), |router| router).await;
    let run_id = RunId::new();
    let key = petri_key(run_id);
    let token = state.test_issue_worker_token(&run_id);
    let launch = OwnerId::new("launch-1");
    let store = HttpRunStore::for_worker(
        worker_client(&base_url, &token, Duration::from_secs(5)).await,
        launch.clone(),
    );

    let created = store
        .open(&key, Access::Create {
            owner: OwnerId::mint(),
        })
        .await
        .expect("the worker creates the run");
    let server_store = state.test_petri_run_store();
    assert_eq!(
        server_store.owner(&key).await.expect("reads the lease"),
        Some(launch.clone()),
        "the lease names the launch"
    );
    let reopened = store
        .open(&key, Access::Write {
            owner: OwnerId::mint(),
        })
        .await
        .expect("the same launch reopens the run");
    assert_eq!(reopened.locator(), created.locator());
    drop(reopened);
    drop(created);
    wait_until_released(server_store, &key).await;
}

/// A worker stores Fabro's platform records for its run over the API and
/// reads them back by kind: what the checkpoint hooks do from the worker
/// process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_appends_and_reads_platform_records_over_the_api() {
    use fabro_petri::platform_records::{HttpPlatformRecords, PlatformRecords};
    use fabro_store::platform_records::{CheckpointRecord, DecisionRef, OperationKey};
    use fabro_store::{PlatformRecord, PlatformRecordKind, StagePosition};

    let state = test_app_state();
    let base_url = serve(Arc::clone(&state), |router| router).await;
    let run_id = RunId::new();
    let token = state.test_issue_worker_token(&run_id);
    let records =
        HttpPlatformRecords::new(worker_client(&base_url, &token, Duration::from_secs(5)).await);

    let checkpoint = PlatformRecord::Checkpoint(CheckpointRecord {
        execution:      0,
        firing:         3,
        attempt:        Some(1),
        workspace:      Some("invocation-0-scope-0".to_string()),
        git_commit_sha: Some("abc123".to_string()),
        diff_summary:   None,
        patch_blob:     None,
        operation:      Some(OperationKey {
            execution: 0,
            decision:  DecisionRef::AttemptStart {
                firing:  3,
                attempt: 1,
            },
            effect:    "checkpoint".to_string(),
        }),
    });
    let position = Some(StagePosition {
        execution: 0,
        firing:    3,
    });
    let stored = records
        .append(&run_id, &checkpoint, position)
        .await
        .expect("the record appends over the API");
    assert_eq!(stored.seq, 1);
    assert_eq!(stored.position, position);
    let notice = PlatformRecord::RunArchived;
    records
        .append(&run_id, &notice, None)
        .await
        .expect("a second record appends");

    let checkpoints = records
        .read_kind(&run_id, PlatformRecordKind::Checkpoint)
        .await
        .expect("the checkpoints read back");
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].seq, 1);
    assert_eq!(checkpoints[0].position, position);
    assert!(matches!(
        &checkpoints[0].record,
        PlatformRecord::Checkpoint(record) if record.git_commit_sha.as_deref() == Some("abc123")
            && record.operation == checkpoint_operation(&checkpoint)
    ));
    let archived = records
        .read_kind(&run_id, PlatformRecordKind::RunArchived)
        .await
        .expect("the archive record reads back");
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].seq, 2);
    assert_eq!(archived[0].position, None);

    // Another run's token cannot read this run's records.
    let other = state.test_issue_worker_token(&RunId::new());
    let foreign =
        HttpPlatformRecords::new(worker_client(&base_url, &other, Duration::from_secs(5)).await);
    assert!(
        foreign
            .read_kind(&run_id, PlatformRecordKind::Checkpoint)
            .await
            .is_err(),
        "a worker token names one run"
    );
}

fn checkpoint_operation(
    record: &fabro_store::PlatformRecord,
) -> Option<fabro_store::platform_records::OperationKey> {
    record.operation().cloned()
}
