mod common;

use serde_json::json;

#[tokio::test]
async fn debug_html_page_serves_valid_html_on_empty_state() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .client
        .get(format!("{}/__debug", server.base_url))
        .send()
        .await
        .expect("debug page request should complete");

    assert_eq!(response.status(), 200);
    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header should be present")
        .to_str()
        .expect("content-type should be valid string");
    assert!(
        content_type.contains("text/html"),
        "content-type should contain text/html, got: {content_type}"
    );

    let body = response.text().await.expect("body should read");
    assert!(
        body.contains("<!DOCTYPE html>"),
        "response should contain DOCTYPE"
    );
    assert!(
        body.contains("twin-openai"),
        "response should contain project name"
    );
    assert!(body.contains("debug"), "response should contain 'debug'");
    assert!(
        body.contains("no active namespaces"),
        "empty state should show 'no active namespaces'"
    );
}

#[tokio::test]
async fn debug_json_endpoint_returns_correct_state_snapshot() {
    let server = common::spawn_server().await.expect("server should start");

    // Load two scenarios: one success, one error
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test" },
                    "script": { "kind": "success" }
                },
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-error" },
                    "script": {
                        "kind": "error",
                        "status": 500,
                        "message": "test error",
                        "error_type": "server_error",
                        "code": "server_error"
                    }
                }
            ]
        }))
        .await;

    // Make one request that consumes the first (success) scenario
    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "hello debug",
            "stream": false
        }))
        .await;
    assert_eq!(response.status(), 200);

    // GET the debug JSON endpoint (unauthenticated)
    let response = server
        .client
        .get(format!("{}/__debug/state.json", server.base_url))
        .send()
        .await
        .expect("debug json request should complete");

    assert_eq!(response.status(), 200);
    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header should be present")
        .to_str()
        .expect("content-type should be valid string");
    assert!(
        content_type.contains("application/json"),
        "content-type should contain application/json, got: {content_type}"
    );

    let body: serde_json::Value = response.json().await.expect("json should parse");

    // Should have a top-level namespaces array
    let namespaces = body["namespaces"]
        .as_array()
        .expect("namespaces should be an array");
    assert_eq!(namespaces.len(), 1, "should have exactly one namespace");

    let ns = &namespaces[0];
    assert!(
        ns["key"].as_str().unwrap().starts_with("Bearer:"),
        "namespace key should start with 'Bearer:', got: {}",
        ns["key"]
    );

    // Should have 1 remaining scenario (the error one; the success was consumed)
    let scenarios = ns["scenarios"]
        .as_array()
        .expect("scenarios should be an array");
    assert_eq!(scenarios.len(), 1, "should have 1 remaining scenario");
    assert_eq!(scenarios[0]["endpoint"], "responses");
    assert_eq!(scenarios[0]["model"], "gpt-error");
    assert_eq!(scenarios[0]["script_kind"], "error");

    // Should have 1 request log
    let request_logs = ns["request_logs"]
        .as_array()
        .expect("request_logs should be an array");
    assert_eq!(request_logs.len(), 1, "should have 1 request log");
    assert_eq!(request_logs[0]["endpoint"], "responses");
    assert_eq!(request_logs[0]["model"], "gpt-test");
    assert!(
        request_logs[0]["input_text"]
            .as_str()
            .unwrap()
            .contains("hello debug"),
        "request log should contain input text 'hello debug'"
    );
}

#[tokio::test]
async fn debug_html_page_reflects_loaded_scenarios_and_request_logs() {
    let server = common::spawn_server().await.expect("server should start");

    // Load one success scenario
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-html-test" },
                    "script": { "kind": "success" }
                }
            ]
        }))
        .await;

    // Make one request with a different model (won't match, but still gets logged
    // via the default behavior)
    let response = server
        .post_responses(json!({
            "model": "gpt-other",
            "input": "check the page",
            "stream": false
        }))
        .await;
    // The request gets a deterministic response (no matching scenario consumed since
    // model doesn't match). Status should be 200 (default behavior).
    assert_eq!(response.status(), 200);

    // GET the debug HTML page
    let response = server
        .client
        .get(format!("{}/__debug", server.base_url))
        .send()
        .await
        .expect("debug page request should complete");

    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("body should read");

    // Should contain the scenario's model name
    assert!(
        body.contains("gpt-html-test"),
        "HTML should contain scenario model 'gpt-html-test'"
    );
    // Should contain the script kind
    assert!(
        body.contains("success"),
        "HTML should contain script kind 'success'"
    );
    // Should contain the request log model
    assert!(
        body.contains("gpt-other"),
        "HTML should contain request log model 'gpt-other'"
    );
    // Should contain the request log input text
    assert!(
        body.contains("check the page"),
        "HTML should contain request log input text 'check the page'"
    );
    // Verify the server-rendered content section does not show empty state.
    // The JS source always includes the "no active namespaces" string as a template,
    // so we check that the server-rendered content div contains namespace sections
    // rather than the empty-state paragraph.
    assert!(
        body.contains("namespace-header"),
        "HTML should contain a namespace-header element (proving non-empty rendering)"
    );
}

#[tokio::test]
async fn debug_routes_not_accessible_when_admin_disabled() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind should succeed");
    let addr = listener.local_addr().expect("should have addr");
    let app = twin_openai::build_app_with_config(twin_openai::config::Config {
        bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
        require_auth: false,
        enable_admin: false,
    });

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server should run");
    });

    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    let html_response = client
        .get(format!("{base_url}/__debug"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(
        html_response.status(),
        404,
        "debug HTML should be 404 when admin disabled"
    );

    let json_response = client
        .get(format!("{base_url}/__debug/state.json"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(
        json_response.status(),
        404,
        "debug JSON should be 404 when admin disabled"
    );
}

#[tokio::test]
async fn debug_page_renders_in_headless_chrome() {
    // Find Chrome binary
    let chrome_binary = ["chromium", "google-chrome", "chromium-browser"]
        .iter()
        .find(|name| {
            std::process::Command::new("which")
                .arg(name)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        });

    let chrome_binary = match chrome_binary {
        Some(name) => *name,
        None => {
            eprintln!("SKIPPED: no Chrome/Chromium binary found on PATH");
            return;
        }
    };

    let server = common::spawn_server().await.expect("server should start");

    // Load a scenario and make a request so the page has content
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-screenshot" },
                    "script": { "kind": "success" }
                }
            ]
        }))
        .await;

    let response = server
        .post_responses(json!({
            "model": "gpt-screenshot",
            "input": "screenshot test",
            "stream": false
        }))
        .await;
    assert_eq!(response.status(), 200);

    let screenshot_path = format!(
        "/tmp/twin-openai-debug-screenshot-{}.png",
        std::process::id()
    );
    let output = std::process::Command::new(chrome_binary)
        .args([
            "--headless",
            "--disable-gpu",
            &format!("--screenshot={screenshot_path}"),
            "--window-size=1280,900",
            &format!("{}/__debug", server.base_url),
        ])
        .output()
        .expect("Chrome should run");

    assert!(
        output.status.success(),
        "Chrome should exit with code 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let screenshot_data = std::fs::read(&screenshot_path).expect("screenshot file should exist");
    assert!(
        screenshot_data.len() >= 10_000,
        "screenshot should be at least 10KB, got {} bytes",
        screenshot_data.len()
    );
    // Check PNG magic bytes
    assert_eq!(
        &screenshot_data[..8],
        b"\x89PNG\r\n\x1a\n",
        "screenshot should be a valid PNG"
    );

    // Clean up
    let _ = std::fs::remove_file(&screenshot_path);
}

#[tokio::test]
async fn debug_html_escapes_user_controlled_values() {
    let server = common::spawn_server().await.expect("server should start");

    // Load a scenario with an XSS attempt in the model name
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": {
                        "endpoint": "responses",
                        "model": "<script>alert('xss')</script>"
                    },
                    "script": { "kind": "success" }
                }
            ]
        }))
        .await;

    let response = server
        .client
        .get(format!("{}/__debug", server.base_url))
        .send()
        .await
        .expect("debug page request should complete");

    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("body should read");

    // Should contain the escaped form
    assert!(
        body.contains("&lt;script&gt;"),
        "HTML should contain escaped '<script>' as '&lt;script&gt;'"
    );
    // Should NOT contain the raw injection
    assert!(
        !body.contains("<script>alert"),
        "HTML should NOT contain raw unescaped '<script>alert'"
    );
}
