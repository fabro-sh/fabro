use std::ffi::OsString;
use std::sync::{LazyLock, Mutex, MutexGuard};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use httpmock::MockServer;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, create_and_start_run_from_manifest, minimal_manifest_json,
    minimal_manifest_json_with_dry_run, test_app_state_with_options, test_app_with_mock_anthropic,
    test_app_with_no_providers, test_app_with_scheduler, test_settings, wait_for_run_status,
};

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct ProxyPolicyGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl ProxyPolicyGuard {
    fn disabled() -> Self {
        let lock = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os(fabro_http::HTTP_PROXY_POLICY_ENV);
        std::env::set_var(fabro_http::HTTP_PROXY_POLICY_ENV, "disabled");
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for ProxyPolicyGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(value) => std::env::set_var(fabro_http::HTTP_PROXY_POLICY_ENV, value),
            None => std::env::remove_var(fabro_http::HTTP_PROXY_POLICY_ENV),
        }
    }
}

fn completion_request(stream: bool) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "messages": [{"role": "user", "content": [{"kind": "text", "data": "Hello"}]}],
                "stream": stream
            }))
            .unwrap(),
        ))
        .unwrap()
}

fn completion_request_with_model(stream: bool, model: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": [{"kind": "text", "data": "Hi"}]}],
                "stream": stream
            }))
            .unwrap(),
        ))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_serve_starts_and_runs_workflow() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;

    let status = wait_for_run_status(&app, &run_id, &["completed", "failed"]).await;
    assert_eq!(status, "completed");
}

#[tokio::test]
async fn test_model_skip_when_no_providers() {
    let app = test_app_with_no_providers();

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/claude-opus-4-6/test"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert_eq!(body["model_id"], "claude-opus-4-6");
    assert_eq!(body["status"], "skip");
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
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dry_run_serve_rejects_invalid_dot() {
    let app = test_app_with_no_providers();

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_manifest_json("not valid dot")).unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn completion_no_provider_non_streaming_returns_502() {
    let app = test_app_with_no_providers();

    let response = app.oneshot(completion_request(false)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn completion_no_provider_streaming_returns_502() {
    let app = test_app_with_no_providers();

    let response = app.oneshot(completion_request(true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn completion_non_streaming_returns_valid_json() {
    let _proxy_guard = ProxyPolicyGuard::disabled();
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
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert!(body["id"].is_string());
    assert_eq!(body["model"], "claude-sonnet-4-5");
    assert_eq!(body["stop_reason"], "end_turn");
    assert!(body["message"].is_object());
    assert!(body["usage"]["input_tokens"].is_number());
    assert!(body["usage"]["output_tokens"].is_number());
}

#[tokio::test]
async fn completion_streaming_returns_sse() {
    let _proxy_guard = ProxyPolicyGuard::disabled();
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
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/event-stream"));
}
