use serde_json::Value;

use super::*;

// Separate receiver paths identify the actual adapter. Body matching (or the
// Gemini URL) checks the wire model, not just the completion response metadata.
async fn assert_completion_route(selector: Value, provider: &str, model: &str, overlay: &str) {
    let upstream = MockServer::start();
    let path = match provider {
        "anthropic" => "/anthropic/v1/messages".to_string(),
        "openai" => "/openai/v1/responses".to_string(),
        "gemini" => format!("/gemini/v1beta/models/{model}:generateContent"),
        _ => unreachable!(),
    };
    let expected = upstream.mock(|when, then| {
        let when = when.method(POST).path(&path);
        if provider != "gemini" {
            when.json_body_includes(json!({"model": model}).to_string());
        }
        let body = match provider {
            "anthropic" => json!({
                "id": "msg-test", "type": "message", "role": "assistant",
                "model": model, "content": [{"type": "text", "text": "OK"}],
                "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 1}
            }),
            "openai" => json!({
                "id": "resp-test", "object": "response", "status": "completed",
                "model": model, "output": [{"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "OK", "annotations": []}]}],
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
            }),
            "gemini" => json!({
                "modelVersion": model,
                "candidates": [{"content": {"role": "model", "parts": [{"text": "OK"}]},
                    "finishReason": "STOP"}],
                "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1, "totalTokenCount": 2}
            }),
            _ => unreachable!(),
        };
        then.status(200).json_body(body);
    });
    let literal_selector = upstream.mock(|when, then| {
        when.method(POST)
            .path("/anthropic/v1/messages")
            .json_body_includes(json!({"model": selector["model"]}).to_string());
        then.status(400)
            .json_body(json!({"error": {"message": "literal selector misroute"}}));
    });
    // A nonretryable failure makes a misroute fail promptly and reports its
    // path and body through httpmock's expected-receipt assertion.
    let unexpected = upstream.mock(|when, then| {
        when.method(POST);
        then.status(400)
            .json_body(json!({"error": {"message": "unexpected route"}}));
    });
    let state = TestAppStateBuilder::new()
        .env_lookup(|_| None)
        .llm_overlay_toml(overlay)
        .provider_base_url("anthropic", upstream.url("/anthropic"))
        .provider_base_url("openai", upstream.url("/openai"))
        .provider_base_url("gemini", upstream.url("/gemini"))
        .vault_entries([
            (EnvVars::ANTHROPIC_API_KEY, "test-anthropic-key"),
            (EnvVars::OPENAI_API_KEY, "test-openai-key"),
            (EnvVars::GEMINI_API_KEY, "test-gemini-key"),
        ])
        .build();
    let app = crate::test_support::build_test_router(state);
    let request = completion_request(&selector);
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        literal_selector.calls(),
        0,
        "selector sent as a literal model to Anthropic: {selector}"
    );
    expected.assert_calls(1);
    unexpected.assert_calls(0);
    fabro_test::assert_axum_status(response, StatusCode::OK, "completion selector routing").await;
}

#[tokio::test]
async fn provider_only_selectors_send_provider_defaults() {
    for (provider, model) in [
        ("gemini", "gemini-3.5-flash"),
        ("openai", "gpt-5.6-sol"),
        ("anthropic", "claude-sonnet-5"),
    ] {
        assert_completion_route(json!({"model": provider}), provider, model, "").await;
    }
}

#[tokio::test]
async fn qualified_and_separate_provider_selectors_keep_their_routes() {
    for (provider, model) in [
        ("gemini", "gemini-3.5-flash"),
        ("openai", "gpt-5.6-sol"),
        ("anthropic", "claude-sonnet-5"),
    ] {
        for selector in [
            json!({"model": format!("{provider}/{model}")}),
            json!({"provider": provider, "model": model}),
            json!({"provider": provider}),
        ] {
            assert_completion_route(selector, provider, model, "").await;
        }
    }
}

fn completion_request(selector: &Value) -> Request<Body> {
    let mut body = json!({
        "stream": false,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
    });
    body.as_object_mut()
        .unwrap()
        .extend(selector.as_object().unwrap().clone());
    Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn provider_alias_uses_the_operator_configured_default() {
    assert_completion_route(
        json!({"model": "google"}),
        "gemini",
        "gemini-3.1-flash-lite",
        "[providers.gemini]\ndefault_model = \"gemini-3.1-flash-lite\"\n",
    )
    .await;
}

#[tokio::test]
async fn unknown_model_still_passes_through_on_the_default_provider() {
    assert_completion_route(
        json!({"model": "future-model"}),
        "anthropic",
        "future-model",
        "",
    )
    .await;
}

#[tokio::test]
async fn invalid_or_unavailable_provider_selection_makes_no_outbound_request() {
    let upstream = MockServer::start();
    let unexpected = upstream.mock(|when, then| {
        when.method(POST);
        then.status(400);
    });
    for (selector, overlay, detail) in [
        (
            json!({"model": "gemini"}),
            "",
            "model provider 'gemini' is unavailable",
        ),
        (
            json!({"model": "google"}),
            "",
            "model provider 'gemini' is unavailable",
        ),
        (
            json!({"model": "gemini/gemini-3.5-flash"}),
            "",
            "model provider 'gemini' is unavailable",
        ),
        (
            json!({"provider": "gemini"}),
            "",
            "model provider 'gemini' is unavailable",
        ),
        (
            json!({"provider": "missing-provider"}),
            "",
            "unknown model provider 'missing-provider'",
        ),
        (
            json!({"model": "gemini"}),
            "[providers.gemini]\nenabled = false\n",
            "unknown model provider 'gemini'",
        ),
    ] {
        let state = TestAppStateBuilder::new()
            .env_lookup(|_| None)
            .llm_overlay_toml(overlay)
            .provider_base_url("anthropic", upstream.url("/anthropic"))
            .provider_base_url("gemini", upstream.url("/gemini"))
            .vault_entries([(EnvVars::ANTHROPIC_API_KEY, "test-anthropic-key")])
            .build();
        let app = crate::test_support::build_test_router(state);
        for stream in [false, true] {
            let mut selector = selector.clone();
            selector["stream"] = json!(stream);
            let response = app
                .clone()
                .oneshot(completion_request(&selector))
                .await
                .unwrap();
            let body = fabro_test::expect_axum_json(
                response,
                StatusCode::BAD_REQUEST,
                "rejected completion selector",
            )
            .await;
            assert_eq!(body["errors"][0]["detail"], detail);
        }
    }
    unexpected.assert_calls(0);
}
