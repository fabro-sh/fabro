//! Wire snapshots for the OpenAI Chat Completions dialect served by
//! `OpenAiCompatibleAdapter` (kimi, zai, minimax, venice, inception, ollama,
//! litellm — all config-only routes over this adapter).

use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::OpenAiCompatibleAdapter;
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

const MODEL: &str = "test-model";

/// Fixed `created` timestamp for canned bodies (named to satisfy clippy's
/// unreadable-literal lint without touching the JSON wire value).
const CREATED_TS: i64 = 1_700_000_000;

/// Minimal valid Chat Completions body for encode-side tests.
fn minimal_body() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": CREATED_TS,
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

fn adapter(server: &MockServer) -> OpenAiCompatibleAdapter {
    OpenAiCompatibleAdapter::new("test-key", server.base_url())
}

/// Runs `complete()` against a capture mock and returns the captured wire
/// request.
async fn encode_capture_with(
    request: &Request,
    configure: impl FnOnce(OpenAiCompatibleAdapter) -> OpenAiCompatibleAdapter,
) -> WireCapture {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(&server, "/chat/completions", minimal_body());
    let adapter = configure(adapter(&server));
    adapter
        .complete(request)
        .await
        .expect("complete should succeed");
    mock.assert();
    take_capture(&slot)
}

async fn encode_capture(request: &Request) -> WireCapture {
    encode_capture_with(request, |adapter| adapter).await
}

/// Runs `stream()` against an SSE transcript and returns the captured wire
/// request plus every emitted stream item as JSON.
async fn stream_capture(
    request: &Request,
    sse_body: &str,
) -> (WireCapture, Vec<serde_json::Value>) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture_sse(&server, "/chat/completions", sse_body);
    let adapter = adapter(&server);
    let events = support::collect_stream_events(&adapter, request).await;
    mock.assert();
    (take_capture(&slot), events)
}

// ---------------------------------------------------------------------------
// Round trip (encode + decode)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn system_and_tools_encode_decode() {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/chat/completions",
        serde_json::json!({
            "id": "chatcmpl_test",
            "object": "chat.completion",
            "created": CREATED_TS,
            "model": MODEL,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello back"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 42, "completion_tokens": 7, "total_tokens": 49}
        }),
    );

    let adapter = adapter(&server);
    let request = Request {
        messages: vec![Message::system("Be concise"), Message::user("Hello")],
        tools: Some(vec![ToolDefinition::function(
            "search",
            "Search files",
            serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )]),
        temperature: Some(0.5),
        ..base_request(MODEL)
    };

    let response = adapter.complete(&request).await.unwrap();
    mock.assert();

    fabro_test::fabro_json_snapshot!(take_capture(&slot), @r#"
    {
      "method": "POST",
      "path": "/chat/completions",
      "headers": [
        [
          "accept",
          "*/*"
        ],
        [
          "authorization",
          "Bearer test-key"
        ],
        [
          "content-length",
          "305"
        ],
        [
          "content-type",
          "application/json"
        ],
        [
          "host",
          "[host]"
        ]
      ],
      "body": {
        "model": "test-model",
        "messages": [
          {
            "role": "system",
            "content": "Be concise"
          },
          {
            "role": "user",
            "content": "Hello"
          }
        ],
        "temperature": 0.5,
        "max_tokens": 128,
        "tools": [
          {
            "type": "function",
            "function": {
              "name": "search",
              "description": "Search files",
              "parameters": {
                "type": "object",
                "properties": {
                  "query": {
                    "type": "string"
                  }
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
      "id": "chatcmpl_test",
      "model": "test-model",
      "provider": "openai-compatible",
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
        "cache_read_tokens": 0,
        "cache_write_tokens": 0
      },
      "raw": {
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "test-model",
        "choices": [
          {
            "index": 0,
            "message": {
              "role": "assistant",
              "content": "Hello back"
            },
            "finish_reason": "stop"
          }
        ],
        "usage": {
          "prompt_tokens": 42,
          "completion_tokens": 7,
          "total_tokens": 49
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
    let capture = encode_capture(&corpus_multi_turn(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "system",
          "content": "You are a terse assistant."
        },
        {
          "role": "user",
          "content": "What is the capital of France?"
        },
        {
          "role": "assistant",
          "content": "Paris."
        },
        {
          "role": "user",
          "content": "And of Spain?"
        }
      ],
      "max_tokens": 128
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_auto() {
    let capture = encode_capture(&corpus_tools(MODEL, Some(ToolChoice::Auto))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128,
      "tools": [
        {
          "type": "function",
          "function": {
            "name": "search",
            "description": "Search files",
            "parameters": {
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
          }
        },
        {
          "type": "function",
          "function": {
            "name": "read_file",
            "description": "Read a file by path",
            "parameters": {
              "type": "object",
              "properties": {
                "path": {
                  "type": "string"
                }
              }
            }
          }
        }
      ],
      "tool_choice": "auto"
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_required() {
    let capture = encode_capture(&corpus_tools(MODEL, Some(ToolChoice::Required))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128,
      "tools": [
        {
          "type": "function",
          "function": {
            "name": "search",
            "description": "Search files",
            "parameters": {
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
          }
        },
        {
          "type": "function",
          "function": {
            "name": "read_file",
            "description": "Read a file by path",
            "parameters": {
              "type": "object",
              "properties": {
                "path": {
                  "type": "string"
                }
              }
            }
          }
        }
      ],
      "tool_choice": "required"
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_named() {
    let capture = encode_capture(&corpus_tools(MODEL, Some(ToolChoice::named("search")))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128,
      "tools": [
        {
          "type": "function",
          "function": {
            "name": "search",
            "description": "Search files",
            "parameters": {
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
          }
        },
        {
          "type": "function",
          "function": {
            "name": "read_file",
            "description": "Read a file by path",
            "parameters": {
              "type": "object",
              "properties": {
                "path": {
                  "type": "string"
                }
              }
            }
          }
        }
      ],
      "tool_choice": {
        "type": "function",
        "function": {
          "name": "search"
        }
      }
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_none() {
    let capture = encode_capture(&corpus_tools(MODEL, Some(ToolChoice::None))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128,
      "tools": [
        {
          "type": "function",
          "function": {
            "name": "search",
            "description": "Search files",
            "parameters": {
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
          }
        },
        {
          "type": "function",
          "function": {
            "name": "read_file",
            "description": "Read a file by path",
            "parameters": {
              "type": "object",
              "properties": {
                "path": {
                  "type": "string"
                }
              }
            }
          }
        }
      ],
      "tool_choice": "none"
    }
    "#);
}

#[tokio::test]
async fn encode_tool_round_trip() {
    let capture = encode_capture(&corpus_tool_round_trip(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Find foo and read /tmp/x"
        },
        {
          "role": "assistant",
          "content": "Let me check.",
          "tool_calls": [
            {
              "id": "call_1",
              "type": "function",
              "function": {
                "name": "search",
                "arguments": "{\"query\":\"foo\"}"
              }
            },
            {
              "id": "call_2",
              "type": "function",
              "function": {
                "name": "read_file",
                "arguments": "{\"path\":\"/tmp/x\"}"
              }
            }
          ]
        },
        {
          "role": "tool",
          "content": "{\"matches\":2}",
          "tool_call_id": "call_1"
        },
        {
          "role": "tool",
          "content": "file not found",
          "tool_call_id": "call_2"
        }
      ],
      "max_tokens": 128,
      "tools": [
        {
          "type": "function",
          "function": {
            "name": "search",
            "description": "Search files",
            "parameters": {
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
          }
        },
        {
          "type": "function",
          "function": {
            "name": "read_file",
            "description": "Read a file by path",
            "parameters": {
              "type": "object",
              "properties": {
                "path": {
                  "type": "string"
                }
              }
            }
          }
        }
      ]
    }
    "#);
}

/// Assistant thinking parts echo back as `reasoning_content` (Kimi-motivated,
/// applies to every compat assistant message).
#[tokio::test]
async fn encode_thinking_round_trip_as_reasoning_content() {
    let capture = encode_capture(&corpus_thinking_round_trip(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Think step by step: what is 2+2?"
        },
        {
          "role": "assistant",
          "content": "4.",
          "reasoning_content": "The user wants 2+2, which is 4."
        },
        {
          "role": "user",
          "content": "Now 3+3?"
        }
      ],
      "max_tokens": 128
    }
    "#);
}

/// The compat encoder performs no attachment I/O: images are dropped
/// outright, documents become fallback text.
#[tokio::test]
async fn encode_inline_attachments() {
    let capture = encode_capture(&corpus_inline_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Describe these attachments.[Document 'report.pdf': content type not supported by this provider]"
        }
      ],
      "max_tokens": 128
    }
    "#);
}

#[tokio::test]
async fn encode_url_attachments() {
    let capture = encode_capture(&corpus_url_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Describe these attachments.[Document 'report.pdf': content type not supported by this provider]"
        }
      ],
      "max_tokens": 128
    }
    "#);
}

#[tokio::test]
async fn encode_bad_file_path_attachments() {
    let capture = encode_capture(&corpus_bad_file_path_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Describe these attachments.[Document 'missing.pdf': content type not supported by this provider]"
        }
      ],
      "max_tokens": 128
    }
    "#);
}

#[tokio::test]
async fn encode_audio_attachment() {
    let capture = encode_capture(&corpus_audio_attachment(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Transcribe this.[Audio content not supported by this provider]"
        }
      ],
      "max_tokens": 128
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
    let capture = encode_capture(&corpus_response_format(MODEL, format)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128,
      "response_format": {
        "type": "json_object"
      }
    }
    "#);
}

#[tokio::test]
async fn encode_response_format_json_schema() {
    let capture = encode_capture(&corpus_response_format(MODEL, json_schema_format())).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128,
      "response_format": {
        "type": "json_schema",
        "json_schema": {
          "name": "response",
          "strict": true,
          "schema": {
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
      }
    }
    "#);
}

#[tokio::test]
async fn encode_sampling_params() {
    let capture = encode_capture(&corpus_sampling_params(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "temperature": 0.7,
      "max_tokens": 128,
      "top_p": 0.9,
      "stop": [
        "END"
      ]
    }
    "#);
}

/// The provider_options namespace key is the runtime adapter NAME, not a
/// static "openai_compatible" key (pinned in-module by
/// `provider_options_uses_adapter_name`; this pins it from outside).
#[tokio::test]
async fn encode_provider_options_keyed_by_adapter_name() {
    let request = corpus_provider_options(
        MODEL,
        serde_json::json!({"kimi": {"repetition_penalty": 1.2}}),
    );
    let capture = encode_capture_with(&request, |adapter| adapter.with_name("kimi")).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128,
      "repetition_penalty": 1.2
    }
    "#);
}

/// Options under a key that does not match the adapter name must not merge.
#[tokio::test]
async fn encode_provider_options_other_namespace_ignored() {
    let request = corpus_provider_options(
        MODEL,
        serde_json::json!({"openai": {"repetition_penalty": 1.2}}),
    );
    let capture = encode_capture_with(&request, |adapter| adapter.with_name("kimi")).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128
    }
    "#);
}

/// The compat adapter has no count-tokens wire route.
#[tokio::test]
async fn count_input_tokens_unavailable() {
    let server = MockServer::start();
    let adapter = adapter(&server);
    let count = adapter
        .count_input_tokens(&base_request(MODEL))
        .await
        .unwrap();
    assert!(count.is_none());
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

async fn decode_response(body: serde_json::Value) -> fabro_llm::types::Response {
    let server = MockServer::start();
    let (mock, _slot) = mount_capture(&server, "/chat/completions", body);
    let adapter = adapter(&server);
    let response = adapter
        .complete(&base_request(MODEL))
        .await
        .expect("complete should succeed");
    mock.assert();
    response
}

#[tokio::test]
async fn decode_tool_calls_with_string_arguments() {
    let response = decode_response(serde_json::json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": CREATED_TS,
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_abc",
                    "type": "function",
                    "function": {"name": "search", "arguments": "{\"query\":\"foo\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 30, "completion_tokens": 12, "total_tokens": 42}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response, @r#"
    {
      "id": "chatcmpl_test",
      "model": "test-model",
      "provider": "openai-compatible",
      "message": {
        "role": "assistant",
        "content": [
          {
            "kind": "tool_call",
            "data": {
              "id": "call_abc",
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
        "input_tokens": 30,
        "output_tokens": 12,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0
      },
      "raw": {
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "test-model",
        "choices": [
          {
            "index": 0,
            "message": {
              "role": "assistant",
              "content": null,
              "tool_calls": [
                {
                  "id": "call_abc",
                  "type": "function",
                  "function": {
                    "name": "search",
                    "arguments": "{\"query\":\"foo\"}"
                  }
                }
              ]
            },
            "finish_reason": "tool_calls"
          }
        ],
        "usage": {
          "prompt_tokens": 30,
          "completion_tokens": 12,
          "total_tokens": 42
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}

#[tokio::test]
async fn decode_reasoning_content_as_thinking() {
    let response = decode_response(serde_json::json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": CREATED_TS,
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "4.",
                "reasoning_content": "The user wants 2+2."
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 25, "completion_tokens": 40, "total_tokens": 65}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response, @r#"
    {
      "id": "chatcmpl_test",
      "model": "test-model",
      "provider": "openai-compatible",
      "message": {
        "role": "assistant",
        "content": [
          {
            "kind": "thinking",
            "data": {
              "text": "The user wants 2+2.",
              "signature": null,
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
        "input_tokens": 25,
        "output_tokens": 40,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0
      },
      "raw": {
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "test-model",
        "choices": [
          {
            "index": 0,
            "message": {
              "role": "assistant",
              "content": "4.",
              "reasoning_content": "The user wants 2+2."
            },
            "finish_reason": "stop"
          }
        ],
        "usage": {
          "prompt_tokens": 25,
          "completion_tokens": 40,
          "total_tokens": 65
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}

/// Compat usage reads only prompt/completion tokens; cached-token details are
/// ignored today (parsing them is a 438-redo behavior change).
#[tokio::test]
async fn decode_usage_ignores_token_details() {
    let response = decode_response(serde_json::json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": CREATED_TS,
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "length"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_tokens_details": {"cached_tokens": 80},
            "completion_tokens_details": {"reasoning_tokens": 20}
        }
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response, @r#"
    {
      "id": "chatcmpl_test",
      "model": "test-model",
      "provider": "openai-compatible",
      "message": {
        "role": "assistant",
        "content": [
          {
            "kind": "text",
            "data": "ok"
          }
        ],
        "name": null,
        "tool_call_id": null
      },
      "finish_reason": "length",
      "usage": {
        "input_tokens": 100,
        "output_tokens": 50,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0
      },
      "raw": {
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "test-model",
        "choices": [
          {
            "index": 0,
            "message": {
              "role": "assistant",
              "content": "ok"
            },
            "finish_reason": "length"
          }
        ],
        "usage": {
          "prompt_tokens": 100,
          "completion_tokens": 50,
          "total_tokens": 150,
          "prompt_tokens_details": {
            "cached_tokens": 80
          },
          "completion_tokens_details": {
            "reasoning_tokens": 20
          }
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
    let sse = support::sse_data_transcript(&[
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":5,"total_tokens":16}}"#,
        "[DONE]",
    ]);
    let (capture, events) = stream_capture(&base_request(MODEL), &sse).await;
    // The captured request pins the stream flag on the wire.
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "model": "test-model",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "max_tokens": 128,
      "stream": true
    }
    "#);
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "text_start",
        "text_id": null
      },
      {
        "type": "text_delta",
        "delta": "Hel",
        "text_id": null
      },
      {
        "type": "text_delta",
        "delta": "lo",
        "text_id": null
      },
      {
        "type": "text_end",
        "text_id": null
      },
      {
        "type": "finish",
        "finish_reason": "stop",
        "usage": {
          "input_tokens": 11,
          "output_tokens": 5,
          "reasoning_tokens": 0,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0
        },
        "response": {
          "id": "chatcmpl_stream",
          "model": "test-model",
          "provider": "openai-compatible",
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
async fn stream_tool_call_deltas() {
    let sse = support::sse_data_transcript(&[
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"search","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"qu"}}]},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ery\":\"foo\"}"}}]},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[],"usage":{"prompt_tokens":20,"completion_tokens":9,"total_tokens":29}}"#,
        "[DONE]",
    ]);
    let (_capture, events) =
        stream_capture(&corpus_tools(MODEL, Some(ToolChoice::Auto)), &sse).await;
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "tool_call_start",
        "tool_call": {
          "id": "call_abc",
          "name": "search",
          "type": "function",
          "arguments": null,
          "raw_arguments": null
        }
      },
      {
        "type": "tool_call_delta",
        "tool_call": {
          "id": "call_abc",
          "name": "search",
          "type": "function",
          "arguments": null,
          "raw_arguments": null
        }
      },
      {
        "type": "tool_call_delta",
        "tool_call": {
          "id": "call_abc",
          "name": "search",
          "type": "function",
          "arguments": null,
          "raw_arguments": null
        }
      },
      {
        "type": "tool_call_end",
        "tool_call": {
          "id": "call_abc",
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
          "id": "chatcmpl_stream",
          "model": "test-model",
          "provider": "openai-compatible",
          "message": {
            "role": "assistant",
            "content": [
              {
                "kind": "tool_call",
                "data": {
                  "id": "call_abc",
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
async fn stream_reasoning_content_deltas() {
    let sse = support::sse_data_transcript(&[
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Let me "},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"reasoning_content":"think"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"content":"4."},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ]);
    let (_capture, events) = stream_capture(&base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "text_start",
        "text_id": null
      },
      {
        "type": "text_delta",
        "delta": "4.",
        "text_id": null
      },
      {
        "type": "text_end",
        "text_id": null
      },
      {
        "type": "finish",
        "finish_reason": "stop",
        "usage": {
          "input_tokens": 0,
          "output_tokens": 0,
          "reasoning_tokens": 0,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0
        },
        "response": {
          "id": "chatcmpl_stream",
          "model": "test-model",
          "provider": "openai-compatible",
          "message": {
            "role": "assistant",
            "content": [
              {
                "kind": "thinking",
                "data": {
                  "text": "Let me think",
                  "signature": null,
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
            "input_tokens": 0,
            "output_tokens": 0,
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

/// Minimax tolerance: a stream that ends without `[DONE]` still synthesizes
/// the finish — but only because content was started.
#[tokio::test]
async fn stream_without_done_synthesizes_finish_when_content_started() {
    let sse = support::sse_data_transcript(&[
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
    ]);
    let (_capture, events) = stream_capture(&base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "text_start",
        "text_id": null
      },
      {
        "type": "text_delta",
        "delta": "Hello",
        "text_id": null
      },
      {
        "type": "text_end",
        "text_id": null
      },
      {
        "type": "finish",
        "finish_reason": "stop",
        "usage": {
          "input_tokens": 0,
          "output_tokens": 0,
          "reasoning_tokens": 0,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0
        },
        "response": {
          "id": "chatcmpl_stream",
          "model": "test-model",
          "provider": "openai-compatible",
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
            "input_tokens": 0,
            "output_tokens": 0,
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

/// The other half of the minimax contract: no content started and no
/// `[DONE]` — nothing is synthesized.
#[tokio::test]
async fn stream_without_done_or_content_synthesizes_nothing() {
    let sse = support::sse_data_transcript(&[
        r#"{"id":"chatcmpl_stream","object":"chat.completion.chunk","created":1700000000,"model":"test-model","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
    ]);
    let (_capture, events) = stream_capture(&base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events, @"[]");
}
