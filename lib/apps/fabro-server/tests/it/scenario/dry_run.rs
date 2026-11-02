use axum::body::Body;
use axum::http::{Request, StatusCode};
use httpmock::MockServer;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, checked_response, create_and_start_run_from_intent,
    minimal_intent_json_with_dry_run, response_json, response_status, test_app_state_with_options,
    test_app_with_mock_anthropic, test_app_with_no_providers, test_app_with_scheduler,
    test_settings, wait_for_run_status,
};

fn completion_request(stream: bool) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "messages": [{"role": "user", "content": [{"type": "text", "text": "Hello"}]}],
                "stream": stream
            }))
            .expect("completion fixture should serialize"),
        ))
        .expect("completion request should build")
}

fn completion_request_with_model(stream: bool, model: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
                "stream": stream
            }))
            .expect("model completion fixture should serialize"),
        ))
        .expect("completion request should build")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_serve_starts_and_runs_workflow() {
    let workspace = tempfile::tempdir().unwrap();
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run_from_intent(
        &app,
        minimal_intent_json_with_dry_run(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");
}

#[tokio::test]
async fn test_model_known_but_unavailable_returns_a_probe_finding() {
    let app = test_app_with_no_providers();

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/claude-opus-4.6/test"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json(
        response,
        StatusCode::OK,
        "POST /api/v1/models/claude-opus-4.6/test",
    )
    .await;
    assert_eq!(body["outcome"]["status"], "failed");
    assert_eq!(body["outcome"]["error"]["kind"], "model_selection");
}

#[tokio::test]
async fn test_model_unknown_via_full_router() {
    let app = test_app_with_no_providers();

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/nonexistent-model-xyz/test"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json(
        response,
        StatusCode::OK,
        "POST /api/v1/models/nonexistent-model-xyz/test",
    )
    .await;
    assert_eq!(body["outcome"]["error"]["kind"], "model_selection");
}

#[tokio::test]
async fn completion_no_provider_non_streaming_returns_400() {
    let app = test_app_with_no_providers();

    let response = app.oneshot(completion_request(false)).await.unwrap();
    response_status(
        response,
        StatusCode::BAD_REQUEST,
        "POST /api/v1/completions",
    )
    .await;
}

#[tokio::test]
async fn completion_no_provider_streaming_returns_400() {
    let app = test_app_with_no_providers();

    let response = app.oneshot(completion_request(true)).await.unwrap();
    response_status(
        response,
        StatusCode::BAD_REQUEST,
        "POST /api/v1/completions?stream=true",
    )
    .await;
}

#[tokio::test]
async fn completion_non_streaming_returns_valid_json() {
    let mock_server = MockServer::start_async().await;
    mock_server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    serde_json::to_string(&serde_json::json!({
                        "id": "msg_test_123",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-4-5",
                        "content": [{"type": "text", "text": "Hello!"}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 10, "output_tokens": 5}
                    }))
                    .unwrap(),
                );
        })
        .await;

    let app = test_app_with_mock_anthropic(&format!("{}/v1", mock_server.url("")));

    let response = app
        .oneshot(completion_request_with_model(false, "claude-sonnet-4-5"))
        .await
        .unwrap();
    let body = response_json(response, StatusCode::OK, "POST /api/v1/completions").await;
    assert!(body["id"].is_string());
    assert_eq!(body["model"]["provider"], "anthropic");
    assert_eq!(body["model"]["model"], "claude-sonnet-4.5");
    assert_eq!(body["finish_reason"], "stop");
    assert!(body["content"].is_array());
    assert!(body["usage"]["input"].is_number());
    assert!(body["usage"]["output"].is_number());
}

/// A provider name in the model field, or a selector the catalog does not
/// know, is refused before any provider is called. Neither may pass through
/// to whichever ready provider wins priority.
#[tokio::test]
async fn completion_with_a_bare_provider_or_unknown_selector_is_rejected_before_any_call() {
    let mock_server = MockServer::start_async().await;
    let anthropic = mock_server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(500);
        })
        .await;
    let app = test_app_with_mock_anthropic(&format!("{}/v1", mock_server.url("")));

    for (model, expected_detail) in [
        ("gemini", "set provider = \"gemini\""),
        ("anthropic", "set provider = \"anthropic\""),
        (
            "not-a-real-model",
            "unknown model selector 'not-a-real-model'",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(completion_request_with_model(false, model))
            .await
            .unwrap();
        let body = response_json(
            response,
            StatusCode::BAD_REQUEST,
            &format!("POST /api/v1/completions with model {model}"),
        )
        .await;
        let detail = body["errors"][0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains(expected_detail),
            "model {model}: unexpected detail {detail:?}"
        );
    }
    assert_eq!(anthropic.calls_async().await, 0);
}

#[tokio::test]
async fn completion_streaming_returns_sse() {
    let mock_server = MockServer::start_async().await;
    mock_server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4-5\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n\
                     event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                     event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
                     event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                     event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
                     event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                );
        })
        .await;

    let app = test_app_with_mock_anthropic(&format!("{}/v1", mock_server.url("")));

    let response = app
        .oneshot(completion_request_with_model(true, "claude-sonnet-4-5"))
        .await
        .unwrap();
    let response = checked_response(response, StatusCode::OK, "POST /api/v1/completions").await;
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/event-stream"));
}
