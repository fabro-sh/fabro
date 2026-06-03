//! Wire snapshots for the Anthropic Messages dialect.

use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::AnthropicAdapter;
use fabro_llm::types::{Message, Request, ToolDefinition};
use httpmock::prelude::*;

use crate::support::{mount_capture, take_capture};

fn base_request(model: &str) -> Request {
    Request {
        model:            model.to_string(),
        messages:         vec![Message::user("Hello")],
        provider:         None,
        tools:            None,
        tool_choice:      None,
        response_format:  None,
        temperature:      None,
        top_p:            None,
        max_tokens:       Some(128),
        stop_sequences:   None,
        reasoning_effort: None,
        speed:            None,
        metadata:         None,
        provider_options: None,
    }
}

#[tokio::test]
async fn system_and_tools_encode_decode() {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/messages",
        serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [{"type": "text", "text": "Hello back"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 42,
                "output_tokens": 7,
                "cache_read_input_tokens": 10,
                "cache_creation_input_tokens": 3
            }
        }),
    );

    let adapter = AnthropicAdapter::new("test-key").with_base_url(server.base_url());
    let request = Request {
        messages: vec![Message::system("Be concise"), Message::user("Hello")],
        tools: Some(vec![ToolDefinition::function(
            "search",
            "Search files",
            serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )]),
        temperature: Some(0.5),
        ..base_request("claude-sonnet-4-20250514")
    };

    let response = adapter.complete(&request).await.unwrap();
    mock.assert();

    fabro_test::fabro_json_snapshot!(take_capture(&slot), @r#"
    {
      "method": "POST",
      "path": "/messages",
      "headers": [
        [
          "accept",
          "*/*"
        ],
        [
          "anthropic-version",
          "2023-06-01"
        ],
        [
          "content-length",
          "316"
        ],
        [
          "content-type",
          "application/json"
        ],
        [
          "host",
          "[host]"
        ],
        [
          "x-api-key",
          "test-key"
        ]
      ],
      "body": {
        "model": "claude-sonnet-4-20250514",
        "messages": [
          {
            "role": "user",
            "content": [
              {
                "type": "text",
                "text": "Hello"
              }
            ]
          }
        ],
        "max_tokens": 128,
        "system": "Be concise",
        "temperature": 0.5,
        "stop_sequences": [],
        "tools": [
          {
            "name": "search",
            "description": "Search files",
            "input_schema": {
              "type": "object",
              "properties": {
                "query": {
                  "type": "string"
                }
              }
            }
          }
        ]
      }
    }
    "#);
    fabro_test::fabro_json_snapshot!(response, @r#"
    {
      "id": "msg_test",
      "model": "claude-sonnet-4-20250514",
      "provider": "anthropic",
      "message": {
        "role": "assistant",
        "content": [
          {
            "kind": "text",
            "data": "Hello back"
          }
        ],
        "name": null,
        "tool_call_id": null
      },
      "finish_reason": "stop",
      "usage": {
        "input_tokens": 42,
        "output_tokens": 7,
        "reasoning_tokens": 0,
        "cache_read_tokens": 10,
        "cache_write_tokens": 3
      },
      "raw": {
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [
          {
            "type": "text",
            "text": "Hello back"
          }
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
          "input_tokens": 42,
          "output_tokens": 7,
          "cache_read_input_tokens": 10,
          "cache_creation_input_tokens": 3
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}
