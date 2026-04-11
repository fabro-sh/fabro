mod common;

use reqwest::header::AUTHORIZATION;
use serde_json::json;

#[tokio::test]
async fn admin_loaded_scenarios_are_consumed_fifo() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": { "kind": "success", "response_text": "first scripted response" }
                },
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": { "kind": "success", "response_text": "second scripted response" }
                }
            ]
        }))
        .await;

    let first = server
        .post_responses(json!({ "model": "gpt-test", "input": "hello", "stream": false }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let second = server
        .post_responses(json!({ "model": "gpt-test", "input": "hello", "stream": false }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let fallback = server
        .post_responses(json!({ "model": "gpt-test", "input": "hello", "stream": false }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(
        first["output"][0]["content"][0]["text"],
        "first scripted response"
    );
    assert_eq!(
        second["output"][0]["content"][0]["text"],
        "second scripted response"
    );
    assert_eq!(
        fallback["output"][0]["content"][0]["text"],
        "deterministic: hello"
    );

    let logs = server.request_logs().await;
    assert_eq!(logs["requests"].as_array().expect("request logs").len(), 3);

    server.reset().await;
    let logs_after_reset = server.request_logs().await;
    assert_eq!(
        logs_after_reset["requests"]
            .as_array()
            .expect("request logs")
            .len(),
        0
    );
}

#[tokio::test]
async fn bearer_namespaces_isolate_scenarios_and_request_logs_on_shared_server() {
    let primary = common::spawn_server().await.expect("server should start");
    let secondary = primary.fork_namespace().expect("secondary namespace");

    primary
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": { "kind": "success", "response_text": "primary scripted response" }
                }
            ]
        }))
        .await;
    secondary
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": { "kind": "success", "response_text": "secondary scripted response" }
                }
            ]
        }))
        .await;

    let primary_response = primary
        .post_responses(
            json!({ "model": "gpt-test", "input": "hello from primary", "stream": false }),
        )
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let secondary_response = secondary
        .post_responses(
            json!({ "model": "gpt-test", "input": "hello from secondary", "stream": false }),
        )
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(
        primary_response["output"][0]["content"][0]["text"],
        "primary scripted response"
    );
    assert_eq!(
        secondary_response["output"][0]["content"][0]["text"],
        "secondary scripted response"
    );

    let primary_logs = primary.request_logs().await;
    let secondary_logs = secondary.request_logs().await;
    assert_eq!(
        primary_logs["requests"]
            .as_array()
            .expect("primary logs")
            .len(),
        1
    );
    assert_eq!(
        secondary_logs["requests"]
            .as_array()
            .expect("secondary logs")
            .len(),
        1
    );
    assert_eq!(
        primary_logs["requests"][0]["input_text"],
        "hello from primary"
    );
    assert_eq!(
        secondary_logs["requests"][0]["input_text"],
        "hello from secondary"
    );
}

#[tokio::test]
async fn admin_reset_only_clears_the_target_bearer_namespace() {
    let primary = common::spawn_server().await.expect("server should start");
    let secondary = primary.fork_namespace().expect("secondary namespace");

    let primary_before_reset = primary
        .post_responses(json!({ "model": "gpt-test", "input": "before reset", "stream": false }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let secondary_before_reset = secondary
        .post_responses(
            json!({ "model": "gpt-test", "input": "before secondary", "stream": false }),
        )
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    secondary
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": { "kind": "success", "response_text": "secondary still scripted" }
                }
            ]
        }))
        .await;

    primary.reset().await;

    let primary_after_reset = primary
        .post_responses(json!({ "model": "gpt-test", "input": "after reset", "stream": false }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let secondary_after_reset = secondary
        .post_responses(
            json!({ "model": "gpt-test", "input": "after secondary reset", "stream": false }),
        )
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(primary_before_reset["id"], "resp_000001");
    assert_eq!(secondary_before_reset["id"], "resp_000001");
    assert_eq!(primary_after_reset["id"], "resp_000001");
    assert_eq!(secondary_after_reset["id"], "resp_000002");
    assert_eq!(
        primary_after_reset["output"][0]["content"][0]["text"],
        "deterministic: after reset"
    );
    assert_eq!(
        secondary_after_reset["output"][0]["content"][0]["text"],
        "secondary still scripted"
    );

    let primary_logs = primary.request_logs().await;
    let secondary_logs = secondary.request_logs().await;
    assert_eq!(
        primary_logs["requests"]
            .as_array()
            .expect("primary logs")
            .len(),
        1
    );
    assert_eq!(
        secondary_logs["requests"]
            .as_array()
            .expect("secondary logs")
            .len(),
        2
    );
    assert_eq!(primary_logs["requests"][0]["input_text"], "after reset");
    assert_eq!(
        secondary_logs["requests"][1]["input_text"],
        "after secondary reset"
    );
}

#[tokio::test]
async fn admin_routes_accept_no_auth_but_reject_invalid_authorization_headers() {
    let server = common::spawn_server().await.expect("server should start");

    let unauthenticated = server
        .client
        .get(format!("{}/__admin/requests", server.base_url))
        .send()
        .await
        .expect("admin logs should complete");
    assert_eq!(unauthenticated.status(), 200);

    for authorization in ["Bearer ", "Basic nope"] {
        let response = server
            .client
            .get(format!("{}/__admin/requests", server.base_url))
            .header(AUTHORIZATION, authorization)
            .send()
            .await
            .expect("admin logs should complete");

        assert_eq!(response.status(), 401);
        let body = response.json::<serde_json::Value>().await.expect("json");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["code"], "missing_bearer_token");
    }
}

#[tokio::test]
async fn responses_supports_scripted_tool_call_and_continuation() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": {
                        "kind": "success",
                        "tool_calls": [
                            {
                                "id": "call_weather",
                                "name": "lookup_weather",
                                "arguments": { "city": "Boston" }
                            }
                        ],
                        "reasoning": ["tool reasoning"]
                    }
                },
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false, "input_contains": "72 and sunny" },
                    "script": { "kind": "success", "response_text": "The weather is 72 and sunny." }
                }
            ]
        }))
        .await;

    let first = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "what is the weather?",
            "stream": false,
            "tools": [{ "type": "function", "name": "lookup_weather" }],
            "tool_choice": {
                "type": "function",
                "name": "lookup_weather"
            }
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(first["output"].as_array().expect("output").len(), 1);
    assert_eq!(first["output"][0]["type"], "function_call");
    assert_eq!(first["output"][0]["name"], "lookup_weather");
    assert_eq!(first["output"][0]["id"], "fc_call_weather");
    assert_eq!(first["output"][0]["call_id"], "call_weather");
    assert_eq!(first["output"][0]["arguments"], "{\"city\":\"Boston\"}");
    assert_eq!(first["reasoning"][0], "tool reasoning");

    let continuation = server
        .post_responses(json!({
            "model": "gpt-test",
            "stream": false,
            "previous_response_id": first["id"],
            "input": [
                {
                    "type": "function_call_output",
                    "call_id": "call_weather",
                    "output": "72 and sunny"
                }
            ]
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(
        continuation["output"][0]["content"][0]["text"],
        "The weather is 72 and sunny."
    );
}

#[tokio::test]
async fn responses_stream_supports_tool_only_turn_without_fabricated_text() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": true },
                    "script": {
                        "kind": "success",
                        "tool_calls": [
                            {
                                "id": "call_weather",
                                "name": "lookup_weather",
                                "arguments": { "city": "Boston" }
                            }
                        ]
                    }
                }
            ]
        }))
        .await;

    let (status, chunks) = server
        .post_responses_stream(json!({
            "model": "gpt-test",
            "input": "what is the weather?",
            "stream": true,
            "tools": [{ "type": "function", "name": "lookup_weather" }],
            "tool_choice": {
                "type": "function",
                "name": "lookup_weather"
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

    assert_eq!(status, 200);
    assert_eq!(
        events,
        vec![
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.output_item.done",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    assert!(joined.contains("\"id\":\"fc_call_weather\""));
    assert!(joined.contains("\"call_id\":\"call_weather\""));
    assert!(joined.contains("\"arguments\":\"{\\\"city\\\":\\\"Boston\\\"}\""));
    assert!(!joined.contains("response.output_text.delta"));
}

#[tokio::test]
async fn responses_structured_output_support_is_explicit() {
    let server = common::spawn_server().await.expect("server should start");

    let json_object = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "structured please",
            "stream": false,
            "text": {
                "format": { "type": "json_object" }
            }
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(
        json_object["output"][0]["content"][1]["type"],
        "output_json"
    );
    assert_eq!(
        json_object["output"][0]["content"][1]["json"]["message"],
        "deterministic: structured please"
    );

    let json_schema = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "schema please",
            "stream": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "response_schema",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "message": { "type": "string" },
                            "ok": { "type": "boolean" }
                        }
                    },
                    "strict": true
                }
            }
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(json_schema["output"][0]["content"][1]["json"]["ok"], true);

    let unsupported = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "schema please",
            "stream": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "response_schema",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "items": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        }
                    }
                }
            }
        }))
        .await;

    assert_eq!(unsupported.status(), 400);

    let primitive_root = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "schema please",
            "stream": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "response_schema",
                    "schema": {
                        "type": "string"
                    }
                }
            }
        }))
        .await;

    assert_eq!(primitive_root.status(), 400);
}

#[tokio::test]
async fn responses_reasoning_and_continuation_fields_round_trip() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "stream": false,
            "previous_response_id": "resp_before",
            "reasoning": { "effort": "medium" },
            "input": [
                { "role": "system", "content": "system prompt" },
                { "role": "assistant", "content": [{ "type": "text", "text": "prior assistant answer" }] },
                { "role": "user", "content": [{ "type": "input_text", "text": "continue carefully" }] },
                { "type": "function_call_output", "call_id": "call_continue", "output": "tool finished" }
            ]
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(
        response["reasoning"][0],
        "reasoning: continue carefully tool finished"
    );
    assert_eq!(
        response["output"][0]["content"][0]["text"],
        "deterministic: continue carefully tool finished"
    );
}

#[tokio::test]
async fn responses_reject_unsupported_text_format_type() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "bad format",
            "text": {
                "format": { "type": "xml" }
            }
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn responses_reject_unsupported_tool_choice_shape() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "bad tools",
            "tools": [{ "type": "function", "name": "lookup_weather" }],
            "tool_choice": { "type": "required" }
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn responses_reject_object_stop_value() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "bad stop",
            "stop": { "type": "object" }
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
}
