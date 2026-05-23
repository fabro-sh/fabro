use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{SecondsFormat, Utc};
use object_store::ObjectStore;
use object_store::memory::InMemory;
use tokio::sync::Barrier;
use tower::ServiceExt;

use crate::helpers::{MINIMAL_DOT, api, minimal_manifest_json, response_json, test_settings};

fn app_with_store(object_store: Arc<dyn ObjectStore>) -> axum::Router {
    let settings = test_settings();
    let store = Arc::new(fabro_store::Database::new(
        Arc::clone(&object_store),
        "event-race",
        Duration::from_millis(1),
        None,
    ));
    let artifact_store = fabro_store::ArtifactStore::new(object_store, "artifacts");
    let state = fabro_server::test_support::TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .env_lookup(|_| None)
        .store_bundle(store, artifact_store)
        .build();
    fabro_server::test_support::build_test_router(state)
}

async fn create_run(app: &axum::Router) -> String {
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_manifest_json(MINIMAL_DOT))
                .expect("manifest should serialize"),
        ))
        .expect("create-run request should build");
    let body = response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::CREATED,
        "POST /api/v1/runs",
    )
    .await;
    body["id"]
        .as_str()
        .expect("create-run response should include an id")
        .to_string()
}

fn append_stage_started_request(run_id: &str, index: usize) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/events")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "id": ulid::Ulid::new().to_string(),
                "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                "run_id": run_id,
                "event": "stage.started",
                "node_id": format!("race-{index}"),
                "node_label": format!("Race {index}"),
                "stage_id": format!("race-{index}@1"),
                "actor": {
                    "kind": "worker",
                    "run_id": run_id,
                },
                "properties": {
                    "index": index,
                    "handler_type": "noop",
                    "attempt": 1,
                    "max_attempts": 1,
                },
            }))
            .expect("event should serialize"),
        ))
        .expect("append-event request should build")
}

async fn append_status_and_body(
    app: axum::Router,
    run_id: String,
    index: usize,
    barrier: Arc<Barrier>,
) -> (usize, StatusCode, String) {
    barrier.wait().await;
    let response = app
        .oneshot(append_stage_started_request(&run_id, index))
        .await
        .expect("append-event response should execute");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("append-event body should buffer");
    (index, status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_event_appends_after_restart_keep_projection_cache_contiguous() {
    let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
    let first_app = app_with_store(Arc::clone(&object_store));
    let run_id = create_run(&first_app).await;

    tokio::time::sleep(Duration::from_millis(25)).await;

    // Simulate a server restart: a fresh AppState opens the existing run with
    // an empty active-run cache, so concurrent appends all race through the
    // public event endpoint instead of sharing an already-open RunDatabase.
    let restarted_app = app_with_store(object_store);
    let appends = 64;
    let barrier = Arc::new(Barrier::new(appends));
    let mut tasks = Vec::with_capacity(appends);
    for index in 0..appends {
        tasks.push(tokio::spawn(append_status_and_body(
            restarted_app.clone(),
            run_id.clone(),
            index,
            Arc::clone(&barrier),
        )));
    }

    let mut results = Vec::with_capacity(appends);
    for task in tasks {
        results.push(task.await.expect("append task should not panic"));
    }
    let failures = results
        .iter()
        .filter(|(_, status, _)| *status != StatusCode::OK)
        .collect::<Vec<_>>();
    assert!(
        failures.is_empty(),
        "all concurrent event appends should succeed, got failures: {failures:#?}"
    );

    let request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/events")))
        .body(Body::empty())
        .expect("list-events request should build");
    let body = response_json(
        restarted_app.clone().oneshot(request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/events"),
    )
    .await;
    let events = body["data"]
        .as_array()
        .expect("events response should include data");
    let seqs = events
        .iter()
        .map(|event| {
            event["seq"]
                .as_u64()
                .expect("event should include numeric seq")
        })
        .collect::<Vec<_>>();

    assert_eq!(
        events.len(),
        appends + 2,
        "every append should be durable; observed seqs: {seqs:?}"
    );
    assert_eq!(
        seqs,
        (1..=u64::try_from(appends + 2).unwrap()).collect::<Vec<_>>(),
        "event seqs should be contiguous"
    );
}
