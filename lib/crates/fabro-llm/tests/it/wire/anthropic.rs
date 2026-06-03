//! Wire snapshots for the Anthropic Messages dialect.

use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::AnthropicAdapter;
use fabro_llm::types::{
    Message, Request, ResponseFormat, ResponseFormatType, ToolChoice, ToolDefinition,
};
use httpmock::prelude::*;

use crate::support::{
    self, WireCapture, base_request, corpus_audio_attachment, corpus_bad_file_path_attachments,
    corpus_inline_attachments, corpus_multi_turn, corpus_provider_options, corpus_response_format,
    corpus_sampling_params, corpus_thinking_round_trip, corpus_tool_round_trip, corpus_tools,
    corpus_url_attachments, json_schema_format, mount_capture, mount_capture_sse, take_capture,
};

const MODEL: &str = "claude-sonnet-4-20250514";

/// Minimal valid Messages API body for encode-side tests that only assert on
/// the captured request.
fn minimal_body() -> serde_json::Value {
    serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

/// Runs `complete()` against a capture mock and returns the captured wire
/// request.
async fn encode_capture(adapter: AnthropicAdapter, request: &Request) -> WireCapture {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(&server, "/messages", minimal_body());
    let adapter = adapter.with_base_url(server.base_url());
    adapter
        .complete(request)
        .await
        .expect("complete should succeed");
    mock.assert();
    take_capture(&slot)
}

/// Runs `stream()` against an SSE transcript and returns the captured wire
/// request plus every emitted stream item as JSON.
async fn stream_capture(
    adapter: AnthropicAdapter,
    request: &Request,
    sse_body: &str,
) -> (WireCapture, Vec<serde_json::Value>) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture_sse(&server, "/messages", sse_body);
    let adapter = adapter.with_base_url(server.base_url());
    let events = support::collect_stream_events(&adapter, request).await;
    mock.assert();
    (take_capture(&slot), events)
}

fn adapter() -> AnthropicAdapter {
    AnthropicAdapter::new("test-key")
}

// ---------------------------------------------------------------------------
// Round trip (encode + decode)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn encode_multi_turn() {
    let capture = encode_capture(adapter(), &corpus_multi_turn(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture, @r#"
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
          "340"
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
                "text": "What is the capital of France?"
              }
            ]
          },
          {
            "role": "assistant",
            "content": [
              {
                "type": "text",
                "text": "Paris."
              }
            ]
          },
          {
            "role": "user",
            "content": [
              {
                "type": "text",
                "text": "And of Spain?"
              }
            ]
          }
        ],
        "max_tokens": 128,
        "system": "You are a terse assistant.",
        "stop_sequences": []
      }
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_auto() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::Auto))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
            },
            "required": [
              "query"
            ]
          }
        },
        {
          "name": "read_file",
          "description": "Read a file by path",
          "input_schema": {
            "type": "object",
            "properties": {
              "path": {
                "type": "string"
              }
            }
          }
        }
      ],
      "tool_choice": {
        "type": "auto"
      }
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_required() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::Required))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
            },
            "required": [
              "query"
            ]
          }
        },
        {
          "name": "read_file",
          "description": "Read a file by path",
          "input_schema": {
            "type": "object",
            "properties": {
              "path": {
                "type": "string"
              }
            }
          }
        }
      ],
      "tool_choice": {
        "type": "any"
      }
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_named() {
    let capture = encode_capture(
        adapter(),
        &corpus_tools(MODEL, Some(ToolChoice::named("search"))),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
            },
            "required": [
              "query"
            ]
          }
        },
        {
          "name": "read_file",
          "description": "Read a file by path",
          "input_schema": {
            "type": "object",
            "properties": {
              "path": {
                "type": "string"
              }
            }
          }
        }
      ],
      "tool_choice": {
        "type": "tool",
        "name": "search"
      }
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_none() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::None))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
      "stop_sequences": []
    }
    "#);
}

#[tokio::test]
async fn encode_tool_round_trip() {
    let capture = encode_capture(adapter(), &corpus_tool_round_trip(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "claude-sonnet-4-20250514",
      "messages": [
        {
          "role": "user",
          "content": [
            {
              "type": "text",
              "text": "Find foo and read /tmp/x"
            }
          ]
        },
        {
          "role": "assistant",
          "content": [
            {
              "type": "text",
              "text": "Let me check."
            },
            {
              "type": "tool_use",
              "id": "call_1",
              "name": "search",
              "input": {
                "query": "foo"
              }
            },
            {
              "type": "tool_use",
              "id": "call_2",
              "name": "read_file",
              "input": {
                "path": "/tmp/x"
              }
            }
          ]
        },
        {
          "role": "user",
          "content": [
            {
              "type": "tool_result",
              "tool_use_id": "call_1",
              "content": "{\"matches\":2}",
              "is_error": false
            },
            {
              "type": "tool_result",
              "tool_use_id": "call_2",
              "content": "file not found",
              "is_error": true
            }
          ]
        }
      ],
      "max_tokens": 128,
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
            },
            "required": [
              "query"
            ]
          }
        },
        {
          "name": "read_file",
          "description": "Read a file by path",
          "input_schema": {
            "type": "object",
            "properties": {
              "path": {
                "type": "string"
              }
            }
          }
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_thinking_round_trip() {
    let capture = encode_capture(adapter(), &corpus_thinking_round_trip(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "claude-sonnet-4-20250514",
      "messages": [
        {
          "role": "user",
          "content": [
            {
              "type": "text",
              "text": "Think step by step: what is 2+2?"
            }
          ]
        },
        {
          "role": "assistant",
          "content": [
            {
              "type": "thinking",
              "thinking": "The user wants 2+2, which is 4.",
              "signature": "sig_test_abc123"
            },
            {
              "type": "text",
              "text": "4."
            }
          ]
        },
        {
          "role": "user",
          "content": [
            {
              "type": "text",
              "text": "Now 3+3?"
            }
          ]
        }
      ],
      "max_tokens": 128,
      "stop_sequences": []
    }
    "#);
}

#[tokio::test]
async fn encode_inline_attachments() {
    let capture = encode_capture(adapter(), &corpus_inline_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "claude-sonnet-4-20250514",
      "messages": [
        {
          "role": "user",
          "content": [
            {
              "type": "text",
              "text": "Describe these attachments."
            },
            {
              "type": "image",
              "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": "ZmFrZS1wbmctYnl0ZXM="
              }
            },
            {
              "type": "document",
              "source": {
                "type": "base64",
                "media_type": "application/pdf",
                "data": "ZmFrZS1wZGYtYnl0ZXM="
              }
            }
          ]
        }
      ],
      "max_tokens": 128,
      "stop_sequences": []
    }
    "#);
}

#[tokio::test]
async fn encode_url_attachments() {
    let capture = encode_capture(adapter(), &corpus_url_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "claude-sonnet-4-20250514",
      "messages": [
        {
          "role": "user",
          "content": [
            {
              "type": "text",
              "text": "Describe these attachments."
            },
            {
              "type": "image",
              "source": {
                "type": "url",
                "url": "https://example.com/picture.png"
              }
            },
            {
              "type": "document",
              "source": {
                "type": "url",
                "url": "https://example.com/report.pdf"
              }
            }
          ]
        }
      ],
      "max_tokens": 128,
      "stop_sequences": []
    }
    "#);
}

#[tokio::test]
async fn encode_bad_file_path_attachments_dropped() {
    let capture = encode_capture(adapter(), &corpus_bad_file_path_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "claude-sonnet-4-20250514",
      "messages": [
        {
          "role": "user",
          "content": [
            {
              "type": "text",
              "text": "Describe these attachments."
            }
          ]
        }
      ],
      "max_tokens": 128,
      "stop_sequences": []
    }
    "#);
}

#[tokio::test]
async fn encode_audio_attachment() {
    let capture = encode_capture(adapter(), &corpus_audio_attachment(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "claude-sonnet-4-20250514",
      "messages": [
        {
          "role": "user",
          "content": [
            {
              "type": "text",
              "text": "Transcribe this."
            },
            {
              "type": "text",
              "text": "[Audio content not supported by this provider]"
            }
          ]
        }
      ],
      "max_tokens": 128,
      "stop_sequences": []
    }
    "#);
}

#[tokio::test]
async fn encode_response_format_json_object() {
    let format = ResponseFormat {
        kind:        ResponseFormatType::JsonObject,
        json_schema: None,
        strict:      false,
    };
    let capture = encode_capture(adapter(), &corpus_response_format(MODEL, format)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
      "system": "You must respond with valid JSON only, no other text.",
      "stop_sequences": []
    }
    "#);
}

#[tokio::test]
async fn encode_response_format_json_schema() {
    let capture = encode_capture(
        adapter(),
        &corpus_response_format(MODEL, json_schema_format()),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
      "stop_sequences": [],
      "tools": [
        {
          "name": "json_output",
          "description": "Output the requested structured data",
          "input_schema": {
            "type": "object",
            "properties": {
              "answer": {
                "type": "string"
              }
            },
            "required": [
              "answer"
            ]
          }
        }
      ],
      "tool_choice": {
        "type": "tool",
        "name": "json_output"
      }
    }
    "#);
}

#[tokio::test]
async fn encode_sampling_params() {
    let capture = encode_capture(adapter(), &corpus_sampling_params(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
      "temperature": 0.7,
      "top_p": 0.9,
      "stop_sequences": [
        "END"
      ],
      "metadata": {
        "trace_id": "trace-123"
      }
    }
    "#);
}

#[tokio::test]
async fn encode_provider_options_anthropic_namespace() {
    let capture = encode_capture(
        adapter(),
        &corpus_provider_options(MODEL, serde_json::json!({"anthropic": {"top_k": 5}})),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
      "stop_sequences": [],
      "top_k": 5
    }
    "#);
}

#[tokio::test]
async fn encode_reasoning_effort_with_levels_catalog() {
    let catalog = support::catalog_from_toml(
        r#"
[providers.anthropic]
display_name = "Anthropic"
adapter = "anthropic"
agent_profile = "anthropic"

[models."test-claude"]
provider = "anthropic"
display_name = "Test Claude"
family = "claude"
default = true

[models."test-claude".limits]
context_window = 200000
max_output = 4096

[models."test-claude".features]
tools = true
vision = true
reasoning = true
reasoning_effort = "levels"
prompt_cache = false
"#,
    );
    let request = Request {
        reasoning_effort: Some(fabro_llm::types::ReasoningEffort::High),
        ..base_request("test-claude")
    };
    let capture = encode_capture(adapter().with_catalog(catalog), &request).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-claude",
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
      "stop_sequences": [],
      "output_config": {
        "effort": "high"
      }
    }
    "#);
}

#[tokio::test]
async fn encode_prompt_cache_with_catalog() {
    let catalog = support::catalog_from_toml(
        r#"
[providers.anthropic]
display_name = "Anthropic"
adapter = "anthropic"
agent_profile = "anthropic"

[models."test-claude"]
provider = "anthropic"
display_name = "Test Claude"
family = "claude"
default = true

[models."test-claude".limits]
context_window = 200000
max_output = 4096

[models."test-claude".features]
tools = true
vision = true
reasoning = true
prompt_cache = true
"#,
    );
    let request = Request {
        messages: vec![
            Message::system("You are a careful reviewer."),
            Message::user("Review this."),
        ],
        ..corpus_tools("test-claude", None)
    };
    // Full capture: the prompt-cache path also controls the beta header.
    let capture = encode_capture(adapter().with_catalog(catalog), &request).await;
    fabro_test::fabro_json_snapshot!(capture, @r#"
    {
      "method": "POST",
      "path": "/messages",
      "headers": [
        [
          "accept",
          "*/*"
        ],
        [
          "anthropic-beta",
          "prompt-caching-2024-07-31"
        ],
        [
          "anthropic-version",
          "2023-06-01"
        ],
        [
          "content-length",
          "559"
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
        "model": "test-claude",
        "messages": [
          {
            "role": "user",
            "content": [
              {
                "type": "text",
                "text": "Review this."
              }
            ]
          }
        ],
        "max_tokens": 128,
        "system": [
          {
            "type": "text",
            "text": "You are a careful reviewer.",
            "cache_control": {
              "type": "ephemeral"
            }
          }
        ],
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
              },
              "required": [
                "query"
              ]
            }
          },
          {
            "name": "read_file",
            "description": "Read a file by path",
            "input_schema": {
              "type": "object",
              "properties": {
                "path": {
                  "type": "string"
                }
              }
            },
            "cache_control": {
              "type": "ephemeral"
            }
          }
        ]
      }
    }
    "#);
}

#[tokio::test]
async fn count_tokens_wire_shape() {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/messages/count_tokens",
        serde_json::json!({"input_tokens": 123}),
    );

    let adapter = adapter().with_base_url(server.base_url());
    let request = Request {
        messages: vec![Message::system("Be concise"), Message::user("Hello")],
        ..corpus_tools(MODEL, None)
    };
    let count = adapter
        .count_input_tokens(&request)
        .await
        .unwrap()
        .expect("anthropic should count tokens");

    mock.assert();
    assert_eq!(count.input_tokens, 123);
    fabro_test::fabro_json_snapshot!(take_capture(&slot), @r#"
    {
      "method": "POST",
      "path": "/messages/count_tokens",
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
          "412"
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
        "system": "Be concise",
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
              },
              "required": [
                "query"
              ]
            }
          },
          {
            "name": "read_file",
            "description": "Read a file by path",
            "input_schema": {
              "type": "object",
              "properties": {
                "path": {
                  "type": "string"
                }
              }
            }
          }
        ]
      }
    }
    "#);
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Runs `complete()` against a canned body and returns the decoded response.
async fn decode_response(body: serde_json::Value) -> fabro_llm::types::Response {
    let server = MockServer::start();
    let (mock, _slot) = mount_capture(&server, "/messages", body);
    let adapter = adapter().with_base_url(server.base_url());
    let response = adapter
        .complete(&base_request(MODEL))
        .await
        .expect("complete should succeed");
    mock.assert();
    response
}

#[tokio::test]
async fn decode_tool_use_stop_reason() {
    let response = decode_response(serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [
            {"type": "text", "text": "Let me search."},
            {
                "type": "tool_use",
                "id": "toolu_01",
                "name": "search",
                "input": {"query": "foo"}
            }
        ],
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {"input_tokens": 30, "output_tokens": 12}
    }))
    .await;
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
            "data": "Let me search."
          },
          {
            "kind": "tool_call",
            "data": {
              "id": "toolu_01",
              "name": "search",
              "type": "function",
              "arguments": {
                "query": "foo"
              },
              "raw_arguments": null
            }
          }
        ],
        "name": null,
        "tool_call_id": null
      },
      "finish_reason": "tool_calls",
      "usage": {
        "input_tokens": 30,
        "output_tokens": 12,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0
      },
      "raw": {
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [
          {
            "type": "text",
            "text": "Let me search."
          },
          {
            "type": "tool_use",
            "id": "toolu_01",
            "name": "search",
            "input": {
              "query": "foo"
            }
          }
        ],
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {
          "input_tokens": 30,
          "output_tokens": 12
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}

#[tokio::test]
async fn decode_thinking_and_redacted_thinking() {
    let response = decode_response(serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [
            {"type": "thinking", "thinking": "Step one.", "signature": "sig_decode_abc"},
            {"type": "redacted_thinking", "data": "opaque-blob"},
            {"type": "text", "text": "Done."}
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 25, "output_tokens": 40}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response, @r#"
    {
      "id": "msg_test",
      "model": "claude-sonnet-4-20250514",
      "provider": "anthropic",
      "message": {
        "role": "assistant",
        "content": [
          {
            "kind": "thinking",
            "data": {
              "text": "Step one.",
              "signature": "sig_decode_abc",
              "redacted": false
            }
          },
          {
            "kind": "redacted_thinking",
            "data": {
              "text": "opaque-blob",
              "signature": null,
              "redacted": true
            }
          },
          {
            "kind": "text",
            "data": "Done."
          }
        ],
        "name": null,
        "tool_call_id": null
      },
      "finish_reason": "stop",
      "usage": {
        "input_tokens": 25,
        "output_tokens": 40,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0
      },
      "raw": {
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [
          {
            "type": "thinking",
            "thinking": "Step one.",
            "signature": "sig_decode_abc"
          },
          {
            "type": "redacted_thinking",
            "data": "opaque-blob"
          },
          {
            "type": "text",
            "text": "Done."
          }
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
          "input_tokens": 25,
          "output_tokens": 40
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}

#[tokio::test]
async fn decode_max_tokens_stop_reason() {
    let response = decode_response(serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [{"type": "text", "text": "Truncated answe"}],
        "stop_reason": "max_tokens",
        "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": 128}
    }))
    .await;
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
            "data": "Truncated answe"
          }
        ],
        "name": null,
        "tool_call_id": null
      },
      "finish_reason": "length",
      "usage": {
        "input_tokens": 10,
        "output_tokens": 128,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0
      },
      "raw": {
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [
          {
            "type": "text",
            "text": "Truncated answe"
          }
        ],
        "stop_reason": "max_tokens",
        "stop_sequence": null,
        "usage": {
          "input_tokens": 10,
          "output_tokens": 128
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_text_happy_path() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_test","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":11,"cache_read_input_tokens":2,"cache_creation_input_tokens":1,"output_tokens":0}}}"#,
        ),
        ("ping", r#"{"type":"ping"}"#),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    let (capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    // The captured request pins the stream flag on the wire.
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
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
      "stop_sequences": [],
      "stream": true
    }
    "#);
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "stream_start"
      },
      {
        "type": "text_start",
        "text_id": "block_0"
      },
      {
        "type": "text_delta",
        "delta": "Hel",
        "text_id": "block_0"
      },
      {
        "type": "text_delta",
        "delta": "lo",
        "text_id": "block_0"
      },
      {
        "type": "text_end",
        "text_id": "block_0"
      },
      {
        "type": "finish",
        "finish_reason": "stop",
        "usage": {
          "input_tokens": 11,
          "output_tokens": 5,
          "reasoning_tokens": 0,
          "cache_read_tokens": 2,
          "cache_write_tokens": 1
        },
        "response": {
          "id": "msg_stream_test",
          "model": "claude-sonnet-4-20250514",
          "provider": "anthropic",
          "message": {
            "role": "assistant",
            "content": [
              {
                "kind": "text",
                "data": "Hello"
              }
            ],
            "name": null,
            "tool_call_id": null
          },
          "finish_reason": "stop",
          "usage": {
            "input_tokens": 11,
            "output_tokens": 5,
            "reasoning_tokens": 0,
            "cache_read_tokens": 2,
            "cache_write_tokens": 1
          },
          "raw": null,
          "warnings": [],
          "rate_limit": null
        }
      }
    ]
    "#);
}

#[tokio::test]
async fn stream_tool_call_deltas() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_tool","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":20,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01","name":"search","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"qu"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ery\":\"foo\"}"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":9}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    let (_capture, events) = stream_capture(
        adapter(),
        &corpus_tools(MODEL, Some(ToolChoice::Auto)),
        &sse,
    )
    .await;
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "stream_start"
      },
      {
        "type": "tool_call_start",
        "tool_call": {
          "id": "toolu_01",
          "name": "search",
          "type": "function",
          "arguments": {},
          "raw_arguments": null
        }
      },
      {
        "type": "tool_call_delta",
        "tool_call": {
          "id": "toolu_01",
          "name": "search",
          "type": "function",
          "arguments": "{\"qu",
          "raw_arguments": null
        }
      },
      {
        "type": "tool_call_delta",
        "tool_call": {
          "id": "toolu_01",
          "name": "search",
          "type": "function",
          "arguments": "ery\":\"foo\"}",
          "raw_arguments": null
        }
      },
      {
        "type": "tool_call_end",
        "tool_call": {
          "id": "toolu_01",
          "name": "search",
          "type": "function",
          "arguments": {
            "query": "foo"
          },
          "raw_arguments": "{\"query\":\"foo\"}"
        }
      },
      {
        "type": "finish",
        "finish_reason": "tool_calls",
        "usage": {
          "input_tokens": 20,
          "output_tokens": 9,
          "reasoning_tokens": 0,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0
        },
        "response": {
          "id": "msg_stream_tool",
          "model": "claude-sonnet-4-20250514",
          "provider": "anthropic",
          "message": {
            "role": "assistant",
            "content": [
              {
                "kind": "tool_call",
                "data": {
                  "id": "toolu_01",
                  "name": "search",
                  "type": "function",
                  "arguments": {
                    "query": "foo"
                  },
                  "raw_arguments": "{\"query\":\"foo\"}"
                }
              }
            ],
            "name": null,
            "tool_call_id": null
          },
          "finish_reason": "tool_calls",
          "usage": {
            "input_tokens": 20,
            "output_tokens": 9,
            "reasoning_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
          },
          "raw": null,
          "warnings": [],
          "rate_limit": null
        }
      }
    ]
    "#);
}

#[tokio::test]
async fn stream_thinking_with_signature_delta() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_think","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":15,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_stream_xyz"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"4."}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":12}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "stream_start"
      },
      {
        "type": "reasoning_start"
      },
      {
        "type": "reasoning_delta",
        "delta": "Let me think"
      },
      {
        "type": "reasoning_end"
      },
      {
        "type": "text_start",
        "text_id": "block_1"
      },
      {
        "type": "text_delta",
        "delta": "4.",
        "text_id": "block_1"
      },
      {
        "type": "text_end",
        "text_id": "block_1"
      },
      {
        "type": "finish",
        "finish_reason": "stop",
        "usage": {
          "input_tokens": 15,
          "output_tokens": 12,
          "reasoning_tokens": 0,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0
        },
        "response": {
          "id": "msg_stream_think",
          "model": "claude-sonnet-4-20250514",
          "provider": "anthropic",
          "message": {
            "role": "assistant",
            "content": [
              {
                "kind": "thinking",
                "data": {
                  "text": "Let me think",
                  "signature": "sig_stream_xyz",
                  "redacted": false
                }
              },
              {
                "kind": "text",
                "data": "4."
              }
            ],
            "name": null,
            "tool_call_id": null
          },
          "finish_reason": "stop",
          "usage": {
            "input_tokens": 15,
            "output_tokens": 12,
            "reasoning_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
          },
          "raw": null,
          "warnings": [],
          "rate_limit": null
        }
      }
    ]
    "#);
}

#[tokio::test]
async fn stream_error_event_mid_stream() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_err","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":9,"output_tokens":0}}}"#,
        ),
        (
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "stream_start"
      },
      {
        "stream_item_error": "Server error from anthropic: Overloaded",
        "retryable": true,
        "failover_eligible": true
      }
    ]
    "#);
}

/// The Anthropic decoder never synthesizes a `Finish` on byte-stream end:
/// `message_stop` is the only finisher. A transcript that ends without it
/// must produce no `Finish` event.
#[tokio::test]
async fn stream_without_message_stop_emits_no_finish() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_cut","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":11,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#,
        ),
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "stream_start"
      },
      {
        "type": "text_start",
        "text_id": "block_0"
      },
      {
        "type": "text_delta",
        "delta": "Hello",
        "text_id": "block_0"
      },
      {
        "type": "text_end",
        "text_id": "block_0"
      }
    ]
    "#);
}
