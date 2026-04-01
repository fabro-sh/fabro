mod common;

use serde_json::json;

#[tokio::test]
async fn chat_completions_non_stream_uses_same_canonical_plan() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_responses(json!({
            "model": "gpt-test",
            "input": "same plan",
            "stream": false
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    let chat = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "same plan" }],
            "stream": false
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(
        response["output"][0]["content"][0]["text"],
        chat["choices"][0]["message"]["content"]
    );
}

#[tokio::test]
async fn chat_completions_stream_uses_same_canonical_plan() {
    let server = common::spawn_server().await.expect("server should start");

    let (status, chunks) = server
        .post_chat_stream(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "stream same plan" }],
            "stream": true
        }))
        .await;

    let joined = chunks.join("");

    assert_eq!(status, 200);
    assert!(joined.contains("\"content\":\"deterministic: stream same plan\""));
    assert!(joined.contains("data: [DONE]"));
}

#[tokio::test]
async fn chat_completions_accepts_supported_openai_compatible_fields() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        { "type": "reasoning", "text": "reasoning trace" }
                    ]
                },
                { "role": "user", "content": "structured chat" }
            ],
            "stream": false,
            "tools": [{ "type": "function", "function": { "name": "lookup" } }],
            "tool_choice": "auto",
            "stop": ["END"],
            "response_format": { "type": "json_object" }
        }))
        .await;

    assert_eq!(response.status(), 200);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "{\"message\":\"deterministic: structured chat\",\"model\":\"gpt-test\"}"
    );
    assert_eq!(
        body["choices"][0]["message"]["reasoning"][0],
        "reasoning: structured chat"
    );
}

#[tokio::test]
async fn chat_completions_supports_scripted_tool_call_and_json_schema() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "chat.completions", "model": "gpt-test", "stream": false, "input_contains": "tool please" },
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
                },
                {
                    "matcher": { "endpoint": "chat.completions", "model": "gpt-test", "stream": true, "input_contains": "tool please" },
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

    let non_stream = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "tool please" }],
            "tools": [{ "type": "function", "function": { "name": "lookup_weather" } }],
            "tool_choice": {
                "type": "function",
                "function": { "name": "lookup_weather" }
            },
            "stream": false
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(non_stream["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        non_stream["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup_weather"
    );
    assert_eq!(non_stream["choices"][0]["message"]["content"], "");

    let (status, chunks) = server
        .post_chat_stream(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "tool please" }],
            "tools": [{ "type": "function", "function": { "name": "lookup_weather" } }],
            "tool_choice": {
                "type": "function",
                "function": { "name": "lookup_weather" }
            },
            "stream": true
        }))
        .await;
    let joined = chunks.join("");
    assert_eq!(status, 200);
    assert!(joined.contains("\"tool_calls\""));
    assert!(!joined.contains("\"content\":\"deterministic:"));

    let structured = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "schema chat" }],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "chat_schema",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "message": { "type": "string" },
                            "ok": { "type": "boolean" }
                        }
                    }
                    ,
                    "strict": true
                }
            },
            "stream": false
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(
        structured["choices"][0]["message"]["content"],
        "{\"message\":\"deterministic: schema chat\",\"ok\":true}"
    );
}

#[tokio::test]
async fn chat_completions_stream_preserves_reasoning_transcript() {
    let server = common::spawn_server().await.expect("server should start");

    let (status, chunks) = server
        .post_chat_stream(json!({
            "model": "gpt-test",
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        { "type": "reasoning", "text": "reasoning trace" }
                    ]
                },
                { "role": "user", "content": "stream same plan" }
            ],
            "stream": true
        }))
        .await;

    let joined = chunks.join("");

    assert_eq!(status, 200);
    assert!(joined.contains("\"reasoning\":\"reasoning: stream same plan\""));
    assert!(joined.contains("\"content\":\"deterministic: stream same plan\""));
    assert!(joined.contains("data: [DONE]"));
}

#[tokio::test]
async fn chat_completions_do_not_infer_reasoning_from_user_text() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "Please explain your reasoning plainly" }],
            "stream": false
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");

    assert_eq!(response["choices"][0]["message"]["reasoning"], json!([]));
    assert_eq!(
        response["choices"][0]["message"]["content"],
        "deterministic: Please explain your reasoning plainly"
    );
}

#[tokio::test]
async fn chat_completions_reject_reasoning_parts_on_non_assistant_messages() {
    let server = common::spawn_server().await.expect("server should start");

    let user_reasoning = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{
                "role": "user",
                "content": [{ "type": "reasoning", "text": "not allowed here" }]
            }],
            "stream": false
        }))
        .await;

    assert_eq!(user_reasoning.status(), 400);
    let body = user_reasoning
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "messages");

    let system_reasoning = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{
                "role": "system",
                "content": [{ "type": "reasoning", "text": "not allowed here either" }]
            }],
            "stream": false
        }))
        .await;

    assert_eq!(system_reasoning.status(), 400);
    let body = system_reasoning
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "messages");
}

#[tokio::test]
async fn chat_completions_reject_unknown_top_level_fields() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "hello" }],
            "unexpected_field": true
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn chat_completions_reject_unsupported_tool_choice_shape() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "hello" }],
            "tools": [{ "type": "function", "function": { "name": "lookup_weather" } }],
            "tool_choice": { "type": "required" }
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn chat_completions_reject_required_tool_choice_without_tools() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "hello" }],
            "tool_choice": "required"
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn chat_completions_reject_unfulfilled_tool_choice_requirements() {
    let server = common::spawn_server().await.expect("server should start");

    let required = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "plain text please" }],
            "tools": [{ "type": "function", "function": { "name": "lookup_weather" } }],
            "tool_choice": "required"
        }))
        .await;

    assert_eq!(required.status(), 400);
    let body = required.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "tool_choice");

    let named = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "plain text please" }],
            "tools": [{ "type": "function", "function": { "name": "lookup_weather" } }],
            "tool_choice": {
                "type": "function",
                "function": { "name": "lookup_weather" }
            }
        }))
        .await;

    assert_eq!(named.status(), 400);
    let body = named.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "tool_choice");
}

#[tokio::test]
async fn chat_completions_rejects_unsupported_response_format() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": "bad format" }],
            "response_format": { "type": "xml" }
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn chat_completions_reject_empty_messages() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": []
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "messages");
}

#[tokio::test]
async fn chat_completions_reject_null_message_content() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{ "role": "user", "content": null }]
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "messages");
}

#[tokio::test]
async fn chat_completions_reject_malformed_image_input() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .post_chat(json!({
            "model": "gpt-test",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {}
                }]
            }]
        }))
        .await;

    assert_eq!(response.status(), 400);
    let body = response.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "messages");
}
