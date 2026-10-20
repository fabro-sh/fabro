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
async fn separate_provider_fields_send_provider_defaults() {
    for (provider, model) in [
        ("gemini", "gemini-3.5-flash"),
        ("openai", "gpt-5.6-sol"),
        ("anthropic", "claude-sonnet-5"),
    ] {
        assert_completion_route(json!({"provider": provider}), provider, model, "").await;
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
            json!({"model": format!("{provider}:{model}")}),
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
        json!({"provider": "google"}),
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
            json!({"provider": "google"}),
            "",
            "provider gemini has no registered adapter",
        ),
        (
            json!({"model": "gemini/gemini-3.5-flash"}),
            "",
            "provider gemini has no registered adapter",
        ),
        (
            json!({"provider": "gemini"}),
            "",
            "provider gemini has no registered adapter",
        ),
        (
            json!({"provider": "missing-provider"}),
            "",
            "provider `missing-provider` was not found",
        ),
        (
            json!({"provider": "gemini"}),
            "[providers.gemini]\nenabled = false\n",
            "provider gemini is disabled in the catalog",
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

#[tokio::test]
async fn inspection_completion_stream_and_count_use_the_same_gemini_wire_model() {
    use fabro_llm::{ModelSelection, selection};
    use lithos_llm::resolver::{AvailableProviders, ModelResolver as _};
    use tokio_stream::StreamExt as _;

    let upstream = MockServer::start();
    let wire = "gemini-3.1-flash-lite";
    let response_body = json!({
        "modelVersion": wire,
        "candidates": [{"content": {"role": "model", "parts": [{"text": "OK"}]}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 1, "totalTokenCount": 8}
    });
    let complete = upstream.mock(|when, then| {
        when.method(POST)
            .path(format!("/gemini/v1beta/models/{wire}:generateContent"));
        then.status(200).json_body(response_body.clone());
    });
    let stream = upstream.mock(|when, then| {
        when.method(POST).path(format!(
            "/gemini/v1beta/models/{wire}:streamGenerateContent"
        ));
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(format!("data: {response_body}\n\n"));
    });
    let count = upstream.mock(|when, then| {
        when.method(POST)
            .path(format!("/gemini/v1beta/models/{wire}:countTokens"));
        then.status(200).json_body(json!({"totalTokens": 7}));
    });
    let unexpected = upstream.mock(|when, then| {
        when.method(POST);
        then.status(400);
    });
    let state = TestAppStateBuilder::new()
        .env_lookup(|_| None)
        .llm_overlay_toml("[providers.gemini]\ndefault_model = \"gemini-3.1-flash-lite\"\n")
        .provider_base_url("gemini", upstream.url("/gemini"))
        .provider_base_url("anthropic", upstream.url("/anthropic"))
        .vault_entries([
            (EnvVars::GEMINI_API_KEY, "fake-gemini"),
            (EnvVars::ANTHROPIC_API_KEY, "fake-anthropic"),
        ])
        .build();
    let catalog = state.catalog();
    let built = state.resolve_llm_client().await.unwrap();
    let eligible = AvailableProviders::new(built.provider_ids());
    for selection in [
        ModelSelection::from_parts(Some("gemini-3.1-flash-lite"), None),
        ModelSelection::from_parts(None, Some("google")),
        ModelSelection::from_parts(None, Some("gemini")),
        ModelSelection::from_parts(Some("gemini:gemini-3.1-flash-lite"), None),
    ] {
        let before_request = selection::policy()
            .resolve_selection(&selection, &catalog, &eligible)
            .unwrap();
        let inspected = built.client.resolve_selection(&selection).unwrap();
        assert_eq!(before_request.handle(), inspected.handle());
        assert_eq!(inspected.provider().id().as_str(), "gemini");
        assert_eq!(inspected.api_model(), wire);
        let request = fabro_llm::Request::builder()
            .selection(selection)
            .user("hi")
            .build()
            .unwrap();
        let response = built.client.complete(request.clone()).await.unwrap();
        assert_eq!(response.model, inspected.handle());
        let mut events = built.client.stream(request.clone()).await.unwrap();
        let mut ended = false;
        while let Some(event) = events.next().await {
            if let fabro_llm::StreamEvent::Ended { response } = event.unwrap() {
                assert_eq!(response.model, inspected.handle());
                ended = true;
            }
        }
        assert!(ended);
        let counted = built
            .client
            .count_input_tokens(request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(counted.tokens(), 7);
        assert_eq!(counted.model(), &inspected.handle());
    }
    // Exercise the HTTP streaming boundary as well as the client's dispatcher.
    let app = crate::test_support::build_test_router(state);
    let response = app
        .oneshot(completion_request(
            &json!({"provider":"gemini", "stream":true}),
        ))
        .await
        .unwrap();
    let body =
        fabro_test::expect_axum_text(response, StatusCode::OK, "stream provider default").await;
    assert!(body.contains("ended"));
    complete.assert_calls(4);
    stream.assert_calls(5);
    count.assert_calls(4);
    unexpected.assert_calls(0);
}

#[tokio::test]
async fn separate_provider_keeps_opaque_model_text_and_catalog_wire_identity() {
    for model in [
        "vendor/model:version",
        "future-model:latest",
        "default",
        "gemini",
    ] {
        assert_completion_route(
            json!({"model":model, "provider":"openai"}),
            "openai",
            model,
            "",
        )
        .await;
    }
    assert_completion_route(
        json!({"model":"friendly", "provider":"openai"}),
        "openai",
        "vendor/model:version",
        r#"
[providers.openai.models.local-model]
display_name = "Local model"
api_model = "vendor/model:version"
aliases = ["friendly"]
capabilities = { text = true }
"#,
    )
    .await;
}

#[tokio::test]
async fn provider_names_in_model_are_rejected_before_dispatch() {
    let upstream = MockServer::start();
    let unexpected = upstream.mock(|when, then| {
        when.method(POST);
        then.status(500);
    });
    let state = TestAppStateBuilder::new()
        .env_lookup(|_| None)
        .provider_base_url("anthropic", upstream.url("/anthropic"))
        .provider_base_url("gemini", upstream.url("/gemini"))
        .vault_entries([
            (EnvVars::ANTHROPIC_API_KEY, "test-anthropic"),
            (EnvVars::GEMINI_API_KEY, "test-gemini"),
        ])
        .build();
    let app = crate::test_support::build_test_router(state);
    for model in ["gemini", "google", "anthropic", "openai", "openrouter"] {
        for stream in [false, true] {
            let response = app
                .clone()
                .oneshot(completion_request(&json!({"model":model,"stream":stream})))
                .await
                .unwrap();
            let body = fabro_test::expect_axum_json(
                response,
                StatusCode::BAD_REQUEST,
                "provider in model",
            )
            .await;
            assert_eq!(
                body["errors"][0]["detail"],
                format!(
                    "`{model}` names a provider, not a model; set provider to `{model}` and omit model to use its default"
                )
            );
        }
    }
    unexpected.assert_calls(0);
}

#[tokio::test]
async fn openrouter_keeps_native_model_ids_and_uses_catalog_wire_ids() {
    let upstream = MockServer::start();
    let state = TestAppStateBuilder::new()
        .env_lookup(|_| None)
        .provider_base_url("openrouter", upstream.url("/openrouter"))
        .vault_entries([("OPENROUTER_API_KEY", "test-openrouter")])
        .build();
    let app = crate::test_support::build_test_router(state);
    for (selector, wire) in [
        (
            json!({"provider":"openrouter", "model":"gemini-3.5-flash"}),
            "google/gemini-3.5-flash",
        ),
        (
            json!({"provider":"openrouter", "model":"google/gemini-3.5-flash"}),
            "google/gemini-3.5-flash",
        ),
        (
            json!({"provider":"openrouter", "model":"some-vendor/new-model:free"}),
            "some-vendor/new-model:free",
        ),
        (
            json!({"provider":"openrouter"}),
            "anthropic/claude-sonnet-5",
        ),
    ] {
        let mut receipt = upstream.mock(|when, then| {
            when.method(POST).path("/openrouter/v1/chat/completions")
                .json_body_includes(json!({"model":wire}).to_string());
            then.status(200).json_body(json!({
                "id":"route", "object":"chat.completion", "model":wire,
                "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]
            }));
        });
        let response = app
            .clone()
            .oneshot(completion_request(&selector))
            .await
            .unwrap();
        fabro_test::assert_axum_status(response, StatusCode::OK, "OpenRouter model scope").await;
        receipt.assert_calls(1);
        receipt.delete();
    }
}
