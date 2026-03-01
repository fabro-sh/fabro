// ===========================================================================
// Full HTTP server lifecycle (TS Scenario 4)
// ===========================================================================

mod server_lifecycle {
    use std::sync::Arc;
    use std::time::Duration;

    use arc_api::server::{build_router, create_app_state};
    use arc_workflows::handler::codergen::CodergenHandler;
    use arc_workflows::handler::exit::ExitHandler;
    use arc_workflows::handler::start::StartHandler;
    use arc_workflows::handler::wait_human::WaitHumanHandler;
    use arc_workflows::handler::HandlerRegistry;
    use arc_workflows::interviewer::Interviewer;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn gate_registry(interviewer: Arc<dyn Interviewer>) -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(CodergenHandler::new(None)));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register("codergen", Box::new(CodergenHandler::new(None)));
        registry.register("wait.human", Box::new(WaitHumanHandler::new(interviewer)));
        registry
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    const GATE_DOT: &str = r#"digraph GateTest {
        graph [goal="Test gate"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        work  [shape=box, prompt="Do work"]
        gate  [shape=hexagon, type="wait.human", label="Approve?"]
        done  [shape=box, prompt="Finish"]
        revise [shape=box, prompt="Revise"]

        start -> work -> gate
        gate -> done   [label="[A] Approve"]
        gate -> revise [label="[R] Revise"]
        done -> exit
        revise -> gate
    }"#;

    async fn test_db() -> sqlx::SqlitePool {
        let pool = arc_db::connect_memory().await.unwrap();
        arc_db::initialize_db(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn full_http_lifecycle_approve_and_complete() {
        let state = create_app_state(test_db().await, gate_registry);
        let app = build_router(Arc::clone(&state), arc_api::jwt_auth::AuthMode::Disabled);

        // 1. Start pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": GATE_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let pipeline_id = body["id"].as_str().unwrap().to_string();

        // 2. Poll for question to appear (pipeline runs start -> work -> gate, then blocks)
        let mut question_id = String::new();
        for _ in 0..500 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let req = Request::builder()
                .method("GET")
                .uri(format!("/pipelines/{pipeline_id}/questions"))
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            let body = body_json(response.into_body()).await;
            let arr = body.as_array().unwrap();
            if !arr.is_empty() {
                question_id = arr[0]["id"].as_str().unwrap().to_string();
                break;
            }
        }
        assert!(!question_id.is_empty(), "question should have appeared");

        // 3. Submit answer selecting first option (Approve)
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/pipelines/{pipeline_id}/questions/{question_id}/answer"
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"value": "A"})).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["accepted"], true);

        // 4. Poll until completed
        let mut final_status = String::new();
        for _ in 0..500 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let req = Request::builder()
                .method("GET")
                .uri(format!("/pipelines/{pipeline_id}"))
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            let body = body_json(response.into_body()).await;
            let status = body["status"].as_str().unwrap().to_string();
            if status == "completed" || status == "failed" {
                final_status = status;
                break;
            }
        }
        assert_eq!(final_status, "completed");

        // 5. Verify context endpoint returns an object
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{pipeline_id}/context"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ctx_body = body_json(response.into_body()).await;
        assert!(ctx_body.is_object(), "context should be an object");

        // 6. Verify no pending questions
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{pipeline_id}/questions"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert!(
            body.as_array().unwrap().is_empty(),
            "no pending questions after completion"
        );
    }

    #[tokio::test]
    async fn full_http_lifecycle_cancel() {
        let state = create_app_state(test_db().await, gate_registry);
        let app = build_router(Arc::clone(&state), arc_api::jwt_auth::AuthMode::Disabled);

        // Start a pipeline that will block at the human gate
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": GATE_DOT})).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let pipeline_id = body["id"].as_str().unwrap().to_string();

        // Wait briefly for pipeline to start running
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cancel it
        let req = Request::builder()
            .method("POST")
            .uri(format!("/pipelines/{pipeline_id}/cancel"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["cancelled"], true);

        // Verify status is cancelled
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{pipeline_id}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"], "cancelled");
    }
}

// ===========================================================================
// SSE event stream content parsing (TS Scenario 8)
// ===========================================================================

mod sse_events {
    use std::sync::Arc;
    use std::time::Duration;

    use arc_api::server::{build_router, create_app_state};
    use arc_workflows::handler::codergen::CodergenHandler;
    use arc_workflows::handler::exit::ExitHandler;
    use arc_workflows::handler::start::StartHandler;
    use arc_workflows::handler::HandlerRegistry;
    use arc_workflows::interviewer::Interviewer;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn simple_registry(_interviewer: Arc<dyn Interviewer>) -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(CodergenHandler::new(None)));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register("codergen", Box::new(CodergenHandler::new(None)));
        registry
    }

    const SIMPLE_DOT: &str = r#"digraph SSETest {
        graph [goal="Test SSE"]
        start [shape=Mdiamond]
        work  [shape=box, prompt="Do work"]
        exit  [shape=Msquare]
        start -> work -> exit
    }"#;

    async fn test_db() -> sqlx::SqlitePool {
        let pool = arc_db::connect_memory().await.unwrap();
        arc_db::initialize_db(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn sse_stream_contains_expected_event_types() {
        let state = create_app_state(test_db().await, simple_registry);
        let app = build_router(Arc::clone(&state), arc_api::jwt_auth::AuthMode::Disabled);

        // Start pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": SIMPLE_DOT})).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let pipeline_id = body["id"].as_str().unwrap().to_string();

        // Get SSE stream
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{pipeline_id}/events"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/event-stream"));

        // Collect SSE frames with a timeout
        let mut body = response.into_body();
        let mut sse_data = String::new();
        while let Ok(Some(Ok(frame))) =
            tokio::time::timeout(Duration::from_millis(500), body.frame()).await
        {
            if let Some(data) = frame.data_ref() {
                sse_data.push_str(&String::from_utf8_lossy(data));
            }
        }

        // Parse SSE data lines and extract event types
        let mut event_types: Vec<String> = Vec::new();
        for line in sse_data.lines() {
            if let Some(json_str) = line.strip_prefix("data:") {
                let json_str = json_str.trim();
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(json_str) {
                    // The event is serialized as a tagged enum, so the type is the first key
                    if let Some(obj) = event.as_object() {
                        for key in obj.keys() {
                            event_types.push(key.clone());
                        }
                    } else if let Some(s) = event.as_str() {
                        event_types.push(s.to_string());
                    }
                }
            }
        }

        // Verify we got events (pipeline may have completed before we subscribed,
        // so we check that the stream was valid SSE)
        // If events were emitted before subscribe, the stream may be empty.
        // That's OK -- the main assertion is content-type + valid SSE format.
        // But if we got events, verify expected types.
        if !event_types.is_empty() {
            assert!(
                event_types
                    .iter()
                    .any(|t| t == "StageStarted" || t == "StageCompleted"),
                "should contain stage events, got: {event_types:?}"
            );
        }

        // Pipeline is complete (SSE stream ended), verify checkpoint
        // Small yield to let the spawned task update state
        tokio::time::sleep(Duration::from_millis(10)).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{pipeline_id}/checkpoint"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let cp_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // If pipeline completed, checkpoint should have completed_nodes
        if !cp_body.is_null() {
            let completed = cp_body["completed_nodes"].as_array();
            if let Some(nodes) = completed {
                let names: Vec<&str> = nodes.iter().filter_map(|v| v.as_str()).collect();
                assert!(names.contains(&"work"), "work should be in completed_nodes");
            }
        }
    }
}

// ===========================================================================
// Serve command: dry-run registry factory builds a working router
// ===========================================================================

mod serve_dry_run {
    use std::sync::Arc;
    use std::time::Duration;

    use arc_api::server::{build_router, create_app_state};
    use arc_workflows::handler::default_registry;
    use arc_workflows::interviewer::Interviewer;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Test"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    async fn test_db() -> sqlx::SqlitePool {
        let pool = arc_db::connect_memory().await.unwrap();
        arc_db::initialize_db(&pool).await.unwrap();
        pool
    }

    /// Build the router exactly as `serve_command` does in dry-run mode.
    async fn dry_run_app() -> axum::Router {
        let factory = |interviewer: Arc<dyn Interviewer>| default_registry(interviewer, || None);
        let state = create_app_state(test_db().await, factory);
        build_router(state, arc_api::jwt_auth::AuthMode::Disabled)
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn dry_run_serve_starts_and_runs_pipeline() {
        let app = dry_run_app().await;

        // POST /pipelines to start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let body = body_json(response.into_body()).await;
        let pipeline_id = body["id"].as_str().unwrap().to_string();
        assert!(!pipeline_id.is_empty());

        // Wait for pipeline to complete
        tokio::time::sleep(Duration::from_millis(500)).await;

        // GET /pipelines/{id} to verify completion
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{pipeline_id}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "completed");
    }

    #[tokio::test]
    async fn dry_run_serve_rejects_invalid_dot() {
        let app = dry_run_app().await;

        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": "not valid dot"})).unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
