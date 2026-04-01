mod common;

use serde_json::json;

#[tokio::test]
async fn healthz_is_available() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .client
        .get(format!("{}/healthz", server.base_url))
        .send()
        .await
        .expect("health request should succeed");

    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn responses_requires_bearer_auth() {
    let server = common::spawn_server().await.expect("server should start");

    let unauthenticated = server
        .client
        .post(format!("{}/v1/responses", server.base_url))
        .json(&json!({
            "model": "gpt-test",
            "input": "hello"
        }))
        .send()
        .await
        .expect("request should complete");

    assert_eq!(unauthenticated.status(), 401);
    let unauthenticated_body = unauthenticated
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(
        unauthenticated_body["error"]["type"],
        "invalid_request_error"
    );
    assert_eq!(
        unauthenticated_body["error"]["code"],
        "missing_bearer_token"
    );

    let authenticated = server
        .auth_client
        .post(format!("{}/v1/responses", server.base_url))
        .json(&json!({
            "model": "gpt-test",
            "input": "hello"
        }))
        .send()
        .await
        .expect("request should complete");

    assert_ne!(authenticated.status(), 401);
}

#[tokio::test]
async fn chat_completions_rejects_empty_bearer_auth() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat_with_auth_header(
            json!({
                "model": "gpt-test",
                "messages": [{ "role": "user", "content": "hello" }]
            }),
            Some("Bearer "),
        )
        .await;

    assert_eq!(response.status(), 401);

    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "missing_bearer_token");
}

#[tokio::test]
async fn chat_completions_requires_bearer_auth() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .client
        .post(format!("{}/v1/chat/completions", server.base_url))
        .json(&json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .send()
        .await
        .expect("request should complete");

    assert_eq!(response.status(), 401);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "missing_bearer_token");
}
