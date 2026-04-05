mod common;

use std::time::Duration;

use reqwest::header::AUTHORIZATION;
use serde_json::json;

#[tokio::test]
async fn scripted_budget_and_content_filter_errors_are_distinct() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "quota" },
                    "script": {
                        "kind": "error",
                        "status": 429,
                        "message": "quota exceeded",
                        "error_type": "insufficient_quota",
                        "code": "quota_exceeded",
                        "retry_after": "120"
                    }
                },
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "filter" },
                    "script": {
                        "kind": "error",
                        "status": 400,
                        "message": "content filtered",
                        "error_type": "content_filter",
                        "code": "content_filter"
                    }
                }
            ]
        }))
        .await;

    let quota = server
        .post_responses(json!({ "model": "gpt-test", "input": "quota please", "stream": false }))
        .await;
    assert_eq!(quota.status(), 429);
    assert_eq!(quota.headers()["retry-after"], "120");
    let quota_body = quota.json::<serde_json::Value>().await.expect("json");
    assert_eq!(quota_body["error"]["type"], "insufficient_quota");

    let filter = server
        .post_responses(json!({ "model": "gpt-test", "input": "filter please", "stream": false }))
        .await;
    assert_eq!(filter.status(), 400);
    let filter_body = filter.json::<serde_json::Value>().await.expect("json");
    assert_eq!(filter_body["error"]["type"], "content_filter");
}

#[tokio::test]
async fn scripted_error_status_matrix_is_preserved() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "400" }, "script": { "kind": "error", "status": 400, "message": "400 error", "error_type": "invalid_request_error", "code": "400_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "401" }, "script": { "kind": "error", "status": 401, "message": "401 error", "error_type": "invalid_api_key", "code": "401_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "403" }, "script": { "kind": "error", "status": 403, "message": "403 error", "error_type": "permission_error", "code": "403_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "404" }, "script": { "kind": "error", "status": 404, "message": "404 error", "error_type": "not_found_error", "code": "404_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "408" }, "script": { "kind": "error", "status": 408, "message": "408 error", "error_type": "timeout_error", "code": "408_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "413" }, "script": { "kind": "error", "status": 413, "message": "413 error", "error_type": "request_too_large", "code": "413_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "429 status" }, "script": { "kind": "error", "status": 429, "message": "429 error", "error_type": "rate_limit_error", "code": "429_code", "retry_after": "30" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "500" }, "script": { "kind": "error", "status": 500, "message": "500 error", "error_type": "server_error", "code": "500_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "502" }, "script": { "kind": "error", "status": 502, "message": "502 error", "error_type": "bad_gateway", "code": "502_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "503" }, "script": { "kind": "error", "status": 503, "message": "503 error", "error_type": "service_unavailable", "code": "503_code" } },
                { "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "504" }, "script": { "kind": "error", "status": 504, "message": "504 error", "error_type": "gateway_timeout", "code": "504_code" } }
            ]
        }))
        .await;

    for status in [400_u16, 401, 403, 404, 408, 413, 429, 500, 502, 503, 504] {
        let marker = if status == 429 {
            "429 status".to_owned()
        } else {
            status.to_string()
        };
        let response = server
            .post_responses(json!({ "model": "gpt-test", "input": marker, "stream": false }))
            .await;
        assert_eq!(response.status().as_u16(), status);
        let headers = response.headers().clone();
        let body = response.json::<serde_json::Value>().await.expect("json");
        assert_eq!(body["error"]["message"], format!("{status} error"));
        if status == 429 {
            assert_eq!(headers["retry-after"], "30");
        }
    }
}

#[tokio::test]
async fn scripted_hang_times_out_client_side() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": { "kind": "hang" }
                }
            ]
        }))
        .await;

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(150))
        .default_headers(
            [(
                AUTHORIZATION,
                server
                    .authorization_header_value()
                    .parse()
                    .expect("valid header"),
            )]
            .into_iter()
            .collect(),
        )
        .build()
        .expect("client");

    let result = client
        .post(format!("{}/v1/responses", server.base_url))
        .json(&json!({ "model": "gpt-test", "input": "hang", "stream": false }))
        .send()
        .await;

    assert!(result.is_err());
    assert!(result.expect_err("timeout").is_timeout());
}

#[tokio::test]
async fn scripted_partial_stream_then_close_is_observable() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": true },
                    "script": {
                        "kind": "success",
                        "response_text": "partial stream",
                        "close_after_chunks": 2
                    }
                }
            ]
        }))
        .await;

    let raw = server
        .post_responses_stream_raw(
            json!({ "model": "gpt-test", "input": "partial", "stream": true }),
        )
        .await;
    let parsed = common::parse_sse_transcript(&raw.body).expect("valid sse prefix");
    let joined = String::from_utf8(raw.body).expect("utf8");

    assert_eq!(raw.status, 200);
    assert_eq!(
        raw.headers
            .get("content-type")
            .expect("content-type header"),
        "text/event-stream"
    );
    assert!(joined.contains("event: response.created"));
    assert!(!joined.contains("event: malformed"));
    assert!(!parsed.done);
}

#[tokio::test]
async fn scripted_partial_stream_then_close_is_observable_for_chat() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "chat.completions", "model": "gpt-test", "stream": true },
                    "script": {
                        "kind": "success",
                        "response_text": "partial chat stream",
                        "close_after_chunks": 2
                    }
                }
            ]
        }))
        .await;

    let raw = server
        .post_chat_stream_raw(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "partial chat" }],
            "stream": true
        }))
        .await;
    let parsed = common::parse_sse_transcript(&raw.body).expect("valid sse prefix");
    let joined = String::from_utf8(raw.body).expect("utf8");

    assert_eq!(raw.status, 200);
    assert!(joined.contains("chat.completion.chunk"));
    assert!(!joined.contains("event: malformed"));
    assert!(!parsed.done);
}

#[tokio::test]
async fn scripted_delayed_first_byte_is_observable() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": true },
                    "script": {
                        "kind": "success",
                        "response_text": "delayed stream",
                        "delay_before_headers_ms": 120
                    }
                }
            ]
        }))
        .await;

    let timed = server
        .post_responses_stream_timed(
            json!({ "model": "gpt-test", "input": "delay", "stream": true }),
        )
        .await;

    assert_eq!(timed.status, 200);
    assert!(timed.first_event_elapsed >= Duration::from_millis(100));
    assert!(timed.chunks.join("").contains("delayed stream"));
}

#[tokio::test]
async fn scripted_malformed_sse_is_observable() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "chat.completions", "model": "gpt-test", "stream": true },
                    "script": {
                        "kind": "success",
                        "response_text": "broken stream",
                        "malformed_sse": true
                    }
                }
            ]
        }))
        .await;

    let raw = server
        .post_chat_stream_raw(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "malformed" }],
            "stream": true
        }))
        .await;
    let joined = String::from_utf8(raw.body.clone()).expect("utf8");
    let parse_error = common::parse_sse_transcript(&raw.body).expect_err("malformed sse");

    assert_eq!(raw.status, 200);
    assert!(joined.contains("event: malformed"));
    assert!(parse_error.contains("incomplete"));
}

#[tokio::test]
async fn scripted_malformed_sse_is_observable_for_responses() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": true },
                    "script": {
                        "kind": "success",
                        "response_text": "broken response stream",
                        "malformed_sse": true
                    }
                }
            ]
        }))
        .await;

    let raw = server
        .post_responses_stream_raw(json!({
            "model": "gpt-test",
            "input": "malformed responses",
            "stream": true
        }))
        .await;
    let joined = String::from_utf8(raw.body.clone()).expect("utf8");
    let parse_error = common::parse_sse_transcript(&raw.body).expect_err("malformed sse");

    assert_eq!(raw.status, 200);
    assert!(joined.contains("event: malformed"));
    assert!(parse_error.contains("incomplete"));
}

#[tokio::test]
async fn localhost_success_paths_stay_fast_enough() {
    let server = common::spawn_server().await.expect("server should start");

    let started = std::time::Instant::now();
    let responses = server
        .post_responses(json!({ "model": "gpt-test", "input": "speed", "stream": false }))
        .await;
    let responses_elapsed = started.elapsed();
    assert_eq!(responses.status(), 200);
    assert!(responses_elapsed < Duration::from_secs(1));

    let started = std::time::Instant::now();
    let timed = server
        .post_responses_stream_timed(
            json!({ "model": "gpt-test", "input": "speed stream", "stream": true }),
        )
        .await;
    let stream_elapsed = started.elapsed();
    assert_eq!(timed.status, 200);
    assert!(timed.first_event_elapsed < Duration::from_secs(1));
    assert!(stream_elapsed < Duration::from_secs(1));
    assert!(!timed.chunks.is_empty());

    let started = std::time::Instant::now();
    let chat = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "speed chat" }],
            "stream": false
        }))
        .await;
    let chat_elapsed = started.elapsed();
    assert_eq!(chat.status(), 200);
    assert!(chat_elapsed < Duration::from_secs(1));
}
