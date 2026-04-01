mod common;

use serde_json::json;

#[tokio::test]
async fn responses_create_returns_deterministic_non_stream_payload() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses_with_headers(
            json!({
                "model": "gpt-test",
                "input": "Hello from the test suite",
                "stream": false
            }),
            Some("org-test"),
            Some("proj-test"),
        )
        .await;

    assert_eq!(response.status(), 200);

    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("json body should parse");

    assert_eq!(body["object"], "response");
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["id"], "resp_000001");
    assert_eq!(body["created"], 1);
    assert_eq!(body["output"][0]["type"], "message");
    assert_eq!(
        body["output"][0]["content"][0]["text"],
        "deterministic: Hello from the test suite"
    );
    assert_eq!(body["usage"]["input_tokens"], 5);
    assert_eq!(body["usage"]["output_tokens"], 5);
}

#[tokio::test]
async fn responses_accepts_supported_openai_request_fields() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "stream": false,
            "input": [
                {
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "Summarize this image please" },
                        { "type": "input_image", "image_url": "https://example.com/cat.png" }
                    ]
                }
            ],
            "metadata": {
                "suite": "responses",
                "case": "supported-fields"
            },
            "stop": ["END"],
            "previous_response_id": "resp_previous",
            "reasoning": {
                "effort": "medium"
            },
            "tools": [
                {
                    "type": "function",
                    "name": "lookup_weather",
                    "description": "Look up weather",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        }
                    }
                }
            ],
            "tool_choice": "auto",
            "text": {
                "format": {
                    "type": "text"
                }
            }
        }))
        .await;

    assert_eq!(response.status(), 200);

    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("json body should parse");

    assert_eq!(body["object"], "response");
    assert_eq!(
        body["output"][0]["content"][0]["text"],
        "deterministic: Summarize this image please"
    );
}

#[tokio::test]
async fn responses_reject_unfulfilled_tool_choice_requirements() {
    let server = common::spawn_server().await.expect("server should start");

    let required = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "plain text please",
            "stream": false,
            "tools": [{ "type": "function", "name": "lookup_weather" }],
            "tool_choice": "required"
        }))
        .await;

    assert_eq!(required.status(), 400);
    let body = required.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "tool_choice");

    let named = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "plain text please",
            "stream": false,
            "tools": [{ "type": "function", "name": "lookup_weather" }],
            "tool_choice": {
                "type": "function",
                "name": "lookup_weather"
            }
        }))
        .await;

    assert_eq!(named.status(), 400);
    let body = named.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "tool_choice");
}

#[tokio::test]
async fn responses_accept_unknown_top_level_fields() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "hello",
            "stream": false,
            "unexpected_field": true
        }))
        .await;

    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn responses_reject_malformed_input_items() {
    let server = common::spawn_server().await.expect("server should start");

    let missing_call_id = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": [
                {
                    "type": "function_call_output",
                    "output": "72 and sunny"
                }
            ],
            "stream": false
        }))
        .await;

    assert_eq!(missing_call_id.status(), 400);
    let body = missing_call_id
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "input");

    let missing_content = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": [
                {
                    "role": "user"
                }
            ],
            "stream": false
        }))
        .await;

    assert_eq!(missing_content.status(), 400);
    let body = missing_content
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "input");
}

#[tokio::test]
async fn responses_reject_malformed_image_input() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": ""
                }]
            }],
            "stream": false
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "input");
}

#[tokio::test]
async fn responses_stream_emits_expected_sse_sequence() {
    let server = common::spawn_server().await.expect("server should start");
    let request = json!({
        "model": "gpt-test",
        "input": "stream this request",
        "stream": true
    });

    let non_stream = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "stream this request",
            "stream": false
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json body should parse");

    let (status, chunks) = server.post_responses_stream(request).await;
    let joined = chunks.join("");
    let transcript = common::parse_sse_transcript(joined.as_bytes()).expect("valid sse");
    let events = transcript
        .events
        .iter()
        .filter_map(|event| event.event.as_deref())
        .collect::<Vec<_>>();

    assert_eq!(status, 200);
    assert_eq!(
        events,
        vec![
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.output_item.done",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    assert!(!transcript.done);
    assert!(joined.contains("deterministic: stream this request"));
    assert!(
        joined.contains(
            non_stream["output"][0]["content"][0]["text"]
                .as_str()
                .expect("text")
        )
    );
}

#[tokio::test]
async fn responses_stream_emits_reasoning_and_completion_events() {
    let server = common::spawn_server().await.expect("server should start");

    let (status, chunks) = server
        .post_responses_stream(json!({
            "model": "gpt-test",
            "input": "show your reasoning",
            "stream": true,
            "reasoning": {
                "effort": "high"
            }
        }))
        .await;

    let joined = chunks.join("");

    assert_eq!(status, 200);
    assert!(joined.contains("event: response.reasoning.delta\n"));
    assert!(joined.contains("reasoning: show your reasoning"));
    assert!(joined.contains("event: response.completed\n"));
}

#[tokio::test]
async fn responses_stream_emits_structured_output_events() {
    let server = common::spawn_server().await.expect("server should start");

    let non_stream = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "structured stream",
            "stream": false,
            "text": {
                "format": { "type": "json_object" }
            }
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    let (status, chunks) = server
        .post_responses_stream(json!({
            "model": "gpt-test",
            "input": "structured stream",
            "stream": true,
            "text": {
                "format": { "type": "json_object" }
            }
        }))
        .await;

    let joined = chunks.join("");
    let transcript = common::parse_sse_transcript(joined.as_bytes()).expect("valid sse");
    let events = transcript
        .events
        .iter()
        .filter_map(|event| event.event.as_deref())
        .collect::<Vec<_>>();
    let streamed_json = transcript
        .events
        .iter()
        .find(|event| event.event.as_deref() == Some("response.output_text.done"))
        .and_then(|event| serde_json::from_str::<serde_json::Value>(&event.data).ok())
        .and_then(|payload| {
            payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .expect("structured stream output text");
    assert_eq!(status, 200);
    assert_eq!(
        events,
        vec![
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.output_item.done",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    assert!(!transcript.done);
    assert_eq!(
        streamed_json["message"],
        non_stream["output"][0]["content"][1]["json"]["message"]
    );
    assert!(
        joined.contains(
            non_stream["output"][0]["content"][1]["json"]["message"]
                .as_str()
                .expect("json message")
        )
    );
}
