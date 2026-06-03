//! Wire snapshots for the Gemini `generateContent` dialect. The model is
//! part of the URL path, auth is the `x-goog-api-key` header, and the
//! decoder mints synthetic UUID tool-call ids (normalized to `[UUID]` in
//! these snapshots).

use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::GeminiAdapter;
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

const MODEL: &str = "gemini-test";
const COMPLETE_PATH: &str = "/models/gemini-test:generateContent";
const STREAM_PATH: &str = "/models/gemini-test:streamGenerateContent";

/// Minimal valid generateContent body for encode-side tests.
fn minimal_body() -> serde_json::Value {
    serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "ok"}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
    })
}

fn adapter() -> GeminiAdapter {
    GeminiAdapter::new("test-key")
}

/// Runs `complete()` against a capture mock and returns the captured wire
/// request.
async fn encode_capture(adapter: GeminiAdapter, request: &Request) -> WireCapture {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(&server, COMPLETE_PATH, minimal_body());
    let adapter = adapter.with_base_url(server.base_url());
    adapter
        .complete(request)
        .await
        .expect("complete should succeed");
    mock.assert();
    take_capture(&slot)
}

/// Runs `stream()` against an SSE transcript and returns the captured wire
/// request plus every emitted stream item as JSON (UUIDs normalized).
async fn stream_capture(
    adapter: GeminiAdapter,
    request: &Request,
    sse_body: &str,
) -> (WireCapture, Vec<serde_json::Value>) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture_sse(&server, STREAM_PATH, sse_body);
    let adapter = adapter.with_base_url(server.base_url());
    let mut events = support::collect_stream_events(&adapter, request).await;
    mock.assert();
    events.iter_mut().for_each(support::normalize_uuids);
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
        COMPLETE_PATH,
        serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Hello back"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 42,
                "candidatesTokenCount": 7,
                "cachedContentTokenCount": 10
            }
        }),
    );

    let adapter = adapter().with_base_url(server.base_url());
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
      "path": "/models/gemini-test:generateContent",
      "headers": [
        [
          "accept",
          "*/*"
        ],
        [
          "content-length",
          "425"
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
          "x-goog-api-key",
          "test-key"
        ]
      ],
      "body": {
        "contents": [
          {
            "role": "user",
            "parts": [
              {
                "text": "Hello"
              }
            ]
          }
        ],
        "systemInstruction": {
          "parts": [
            {
              "text": "Be concise"
            }
          ]
        },
        "generationConfig": {
          "temperature": 0.5,
          "maxOutputTokens": 128
        },
        "tools": [
          {
            "functionDeclarations": [
              {
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
            ]
          }
        ],
        "safety_settings": [
          {
            "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
            "threshold": "BLOCK_ONLY_HIGH"
          }
        ]
      }
    }
    "#);
    // Normalized like the decode helpers: gemini mints a synthetic UUID for
    // the response id.
    let mut response_value = serde_json::to_value(&response).expect("response should serialize");
    support::normalize_uuids(&mut response_value);
    fabro_test::fabro_json_snapshot!(response_value, @r#"
    {
      "id": "[UUID]",
      "model": "gemini-test",
      "provider": "gemini",
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
        "input_tokens": 32,
        "output_tokens": 7,
        "reasoning_tokens": 0,
        "cache_read_tokens": 10,
        "cache_write_tokens": 0
      },
      "raw": {
        "candidates": [
          {
            "content": {
              "role": "model",
              "parts": [
                {
                  "text": "Hello back"
                }
              ]
            },
            "finishReason": "STOP"
          }
        ],
        "usageMetadata": {
          "promptTokenCount": 42,
          "candidatesTokenCount": 7,
          "cachedContentTokenCount": 10
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
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "What is the capital of France?"
            }
          ]
        },
        {
          "role": "model",
          "parts": [
            {
              "text": "Paris."
            }
          ]
        },
        {
          "role": "user",
          "parts": [
            {
              "text": "And of Spain?"
            }
          ]
        }
      ],
      "systemInstruction": {
        "parts": [
          {
            "text": "You are a terse assistant."
          }
        ]
      },
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_auto() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::Auto))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "tools": [
        {
          "functionDeclarations": [
            {
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
            },
            {
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
          ]
        }
      ],
      "toolConfig": {
        "functionCallingConfig": {
          "mode": "AUTO"
        }
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_required() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::Required))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "tools": [
        {
          "functionDeclarations": [
            {
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
            },
            {
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
          ]
        }
      ],
      "toolConfig": {
        "functionCallingConfig": {
          "mode": "ANY"
        }
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
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
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "tools": [
        {
          "functionDeclarations": [
            {
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
            },
            {
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
          ]
        }
      ],
      "toolConfig": {
        "functionCallingConfig": {
          "mode": "ANY",
          "allowedFunctionNames": [
            "search"
          ]
        }
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_tool_choice_none() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::None))).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "tools": [
        {
          "functionDeclarations": [
            {
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
            },
            {
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
          ]
        }
      ],
      "toolConfig": {
        "functionCallingConfig": {
          "mode": "NONE"
        }
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_tool_round_trip() {
    let capture = encode_capture(adapter(), &corpus_tool_round_trip(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Find foo and read /tmp/x"
            }
          ]
        },
        {
          "role": "model",
          "parts": [
            {
              "text": "Let me check."
            },
            {
              "functionCall": {
                "name": "search",
                "args": {
                  "query": "foo"
                }
              }
            },
            {
              "functionCall": {
                "name": "read_file",
                "args": {
                  "path": "/tmp/x"
                }
              }
            }
          ]
        },
        {
          "role": "user",
          "parts": [
            {
              "functionResponse": {
                "name": "search",
                "response": {
                  "matches": 2
                }
              }
            }
          ]
        },
        {
          "role": "user",
          "parts": [
            {
              "functionResponse": {
                "name": "read_file",
                "response": {
                  "result": "file not found"
                }
              }
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "tools": [
        {
          "functionDeclarations": [
            {
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
            },
            {
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
          ]
        }
      ],
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
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
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Think step by step: what is 2+2?"
            }
          ]
        },
        {
          "role": "model",
          "parts": [
            {
              "text": "4."
            }
          ]
        },
        {
          "role": "user",
          "parts": [
            {
              "text": "Now 3+3?"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_inline_attachments() {
    let capture = encode_capture(adapter(), &corpus_inline_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Describe these attachments."
            },
            {
              "inlineData": {
                "mimeType": "image/png",
                "data": "ZmFrZS1wbmctYnl0ZXM="
              }
            },
            {
              "inlineData": {
                "mimeType": "application/pdf",
                "data": "ZmFrZS1wZGYtYnl0ZXM="
              }
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_url_attachments() {
    let capture = encode_capture(adapter(), &corpus_url_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Describe these attachments."
            },
            {
              "fileData": {
                "mimeType": "image/png",
                "fileUri": "https://example.com/picture.png"
              }
            },
            {
              "fileData": {
                "mimeType": "application/pdf",
                "fileUri": "https://example.com/report.pdf"
              }
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_bad_file_path_attachments_dropped() {
    let capture = encode_capture(adapter(), &corpus_bad_file_path_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Describe these attachments."
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

/// Gemini sends inline audio (the only dialect that does).
#[tokio::test]
async fn encode_audio_attachment() {
    let capture = encode_capture(adapter(), &corpus_audio_attachment(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Transcribe this."
            },
            {
              "inlineData": {
                "mimeType": "audio/wav",
                "data": "ZmFrZS13YXYtYnl0ZXM="
              }
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
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
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128,
        "responseMimeType": "application/json"
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
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
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128,
        "responseMimeType": "application/json",
        "responseSchema": {
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
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_sampling_params() {
    let capture = encode_capture(adapter(), &corpus_sampling_params(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "temperature": 0.7,
        "maxOutputTokens": 128,
        "topP": 0.9,
        "stopSequences": [
          "END"
        ]
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

/// The "gemini"-namespaced provider_options merge — and the default
/// safety_settings injection it can override.
#[tokio::test]
async fn encode_provider_options_gemini_namespace() {
    let capture = encode_capture(
        adapter(),
        &corpus_provider_options(
            MODEL,
            serde_json::json!({"gemini": {"cached_content": "cachedContents/abc"}}),
        ),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "cached_content": "cachedContents/abc",
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn encode_provider_options_can_override_safety_settings() {
    let capture = encode_capture(
        adapter(),
        &corpus_provider_options(
            MODEL,
            serde_json::json!({"gemini": {"safety_settings": []}}),
        ),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "safety_settings": []
    }
    "#);
}

#[tokio::test]
async fn encode_reasoning_effort_with_levels_catalog() {
    let catalog = support::catalog_from_toml(
        r#"
[providers.gemini]
display_name = "Gemini"
adapter = "gemini"
agent_profile = "gemini"

[models."gemini-test"]
provider = "gemini"
display_name = "Test Gemini"
family = "gemini"
default = true

[models."gemini-test".limits]
context_window = 200000
max_output = 4096

[models."gemini-test".features]
tools = true
vision = true
reasoning = true
reasoning_effort = "levels"
"#,
    );
    let request = Request {
        reasoning_effort: Some(fabro_llm::types::ReasoningEffort::High),
        ..base_request(MODEL)
    };
    let capture = encode_capture(adapter().with_catalog(catalog), &request).await;
    fabro_test::fabro_json_snapshot!(capture.body, @r#"
    {
      "contents": [
        {
          "role": "user",
          "parts": [
            {
              "text": "Hello"
            }
          ]
        }
      ],
      "generationConfig": {
        "maxOutputTokens": 128
      },
      "safety_settings": [
        {
          "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
          "threshold": "BLOCK_ONLY_HIGH"
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn count_tokens_wire_shape() {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/models/gemini-test:countTokens",
        serde_json::json!({"totalTokens": 123}),
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
        .expect("gemini should count tokens");

    mock.assert();
    assert_eq!(count.input_tokens, 123);
    fabro_test::fabro_json_snapshot!(take_capture(&slot), @r#"
    {
      "method": "POST",
      "path": "/models/gemini-test:countTokens",
      "headers": [
        [
          "accept",
          "*/*"
        ],
        [
          "content-length",
          "583"
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
          "x-goog-api-key",
          "test-key"
        ]
      ],
      "body": {
        "generateContentRequest": {
          "contents": [
            {
              "role": "user",
              "parts": [
                {
                  "text": "Hello"
                }
              ]
            }
          ],
          "systemInstruction": {
            "parts": [
              {
                "text": "Be concise"
              }
            ]
          },
          "generationConfig": {
            "maxOutputTokens": 128
          },
          "tools": [
            {
              "functionDeclarations": [
                {
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
                },
                {
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
              ]
            }
          ],
          "safety_settings": [
            {
              "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
              "threshold": "BLOCK_ONLY_HIGH"
            }
          ]
        }
      }
    }
    "#);
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Runs `complete()` against a canned body and returns the decoded response
/// as JSON with synthetic UUIDs normalized.
async fn decode_response(body: serde_json::Value) -> serde_json::Value {
    let server = MockServer::start();
    let (mock, _slot) = mount_capture(&server, COMPLETE_PATH, body);
    let adapter = adapter().with_base_url(server.base_url());
    let response = adapter
        .complete(&base_request(MODEL))
        .await
        .expect("complete should succeed");
    mock.assert();
    let mut value = serde_json::to_value(&response).expect("response should serialize");
    support::normalize_uuids(&mut value);
    value
}

/// functionCall parts get synthetic UUID ids, preserve `thoughtSignature`,
/// and force the finish reason to ToolCalls regardless of `finishReason`.
#[tokio::test]
async fn decode_function_call_with_thought_signature() {
    let response = decode_response(serde_json::json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "Let me search."},
                    {
                        "functionCall": {"name": "search", "args": {"query": "foo"}},
                        "thoughtSignature": "sig_gemini_xyz"
                    }
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 30, "candidatesTokenCount": 12}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response, @r#"
    {
      "id": "[UUID]",
      "model": "gemini-test",
      "provider": "gemini",
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
              "id": "[UUID]",
              "name": "search",
              "type": "function",
              "arguments": {
                "query": "foo"
              },
              "raw_arguments": null,
              "provider_metadata": {
                "thoughtSignature": "sig_gemini_xyz"
              }
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
        "candidates": [
          {
            "content": {
              "role": "model",
              "parts": [
                {
                  "text": "Let me search."
                },
                {
                  "functionCall": {
                    "name": "search",
                    "args": {
                      "query": "foo"
                    }
                  },
                  "thoughtSignature": "sig_gemini_xyz"
                }
              ]
            },
            "finishReason": "STOP"
          }
        ],
        "usageMetadata": {
          "promptTokenCount": 30,
          "candidatesTokenCount": 12
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}

/// The Gemini usage arithmetic: input = (prompt - cached) + tool_use_prompt;
/// thoughts become reasoning tokens.
#[tokio::test]
async fn decode_usage_arithmetic() {
    let response = decode_response(serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "ok"}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 50,
            "thoughtsTokenCount": 8,
            "cachedContentTokenCount": 30,
            "toolUsePromptTokenCount": 5
        }
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response, @r#"
    {
      "id": "[UUID]",
      "model": "gemini-test",
      "provider": "gemini",
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
      "finish_reason": "stop",
      "usage": {
        "input_tokens": 75,
        "output_tokens": 50,
        "reasoning_tokens": 8,
        "cache_read_tokens": 30,
        "cache_write_tokens": 0
      },
      "raw": {
        "candidates": [
          {
            "content": {
              "role": "model",
              "parts": [
                {
                  "text": "ok"
                }
              ]
            },
            "finishReason": "STOP"
          }
        ],
        "usageMetadata": {
          "promptTokenCount": 100,
          "candidatesTokenCount": 50,
          "thoughtsTokenCount": 8,
          "cachedContentTokenCount": 30,
          "toolUsePromptTokenCount": 5
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}

/// `thought: true` text parts decode as Thinking content.
#[tokio::test]
async fn decode_thought_parts() {
    let response = decode_response(serde_json::json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "Adding the numbers.", "thought": true},
                    {"text": "4."}
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 25, "candidatesTokenCount": 40}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response, @r#"
    {
      "id": "[UUID]",
      "model": "gemini-test",
      "provider": "gemini",
      "message": {
        "role": "assistant",
        "content": [
          {
            "kind": "thinking",
            "data": {
              "text": "Adding the numbers.",
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
        "candidates": [
          {
            "content": {
              "role": "model",
              "parts": [
                {
                  "text": "Adding the numbers.",
                  "thought": true
                },
                {
                  "text": "4."
                }
              ]
            },
            "finishReason": "STOP"
          }
        ],
        "usageMetadata": {
          "promptTokenCount": 25,
          "candidatesTokenCount": 40
        }
      },
      "warnings": [],
      "rate_limit": null
    }
    "#);
}

#[tokio::test]
async fn decode_max_tokens_and_safety_finish_reasons() {
    let length = decode_response(serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "Trunc"}]},
            "finishReason": "MAX_TOKENS"
        }],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 128}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(length["finish_reason"], @r#""length""#);

    let safety = decode_response(serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": ""}]},
            "finishReason": "SAFETY"
        }],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 0}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(safety["finish_reason"], @r#""content_filter""#);
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_text_happy_path() {
    let sse = support::sse_data_transcript(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hel"}]}}]}"#,
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":5}}"#,
    ]);
    let (capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    // The captured request pins model-in-URL and `?alt=sse` on the wire.
    fabro_test::fabro_json_snapshot!(capture, @r#"
    {
      "method": "POST",
      "path": "/models/gemini-test:streamGenerateContent?alt=sse",
      "headers": [
        [
          "accept",
          "*/*"
        ],
        [
          "content-length",
          "197"
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
          "x-goog-api-key",
          "test-key"
        ]
      ],
      "body": {
        "contents": [
          {
            "role": "user",
            "parts": [
              {
                "text": "Hello"
              }
            ]
          }
        ],
        "generationConfig": {
          "maxOutputTokens": 128
        },
        "safety_settings": [
          {
            "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
            "threshold": "BLOCK_ONLY_HIGH"
          }
        ]
      }
    }
    "#);
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "stream_start"
      },
      {
        "type": "text_start",
        "text_id": "[UUID]"
      },
      {
        "type": "text_delta",
        "delta": "Hel",
        "text_id": "[UUID]"
      },
      {
        "type": "text_delta",
        "delta": "lo",
        "text_id": "[UUID]"
      },
      {
        "type": "text_end",
        "text_id": "[UUID]"
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
          "id": "[UUID]",
          "model": "gemini-test",
          "provider": "gemini",
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
async fn stream_function_call() {
    let sse = support::sse_data_transcript(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"search","args":{"query":"foo"}},"thoughtSignature":"sig_stream_g"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":20,"candidatesTokenCount":9}}"#,
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
          "id": "[UUID]",
          "name": "search",
          "type": "function",
          "arguments": {
            "query": "foo"
          },
          "raw_arguments": null,
          "provider_metadata": {
            "thoughtSignature": "sig_stream_g"
          }
        }
      },
      {
        "type": "tool_call_end",
        "tool_call": {
          "id": "[UUID]",
          "name": "search",
          "type": "function",
          "arguments": {
            "query": "foo"
          },
          "raw_arguments": null,
          "provider_metadata": {
            "thoughtSignature": "sig_stream_g"
          }
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
          "id": "[UUID]",
          "model": "gemini-test",
          "provider": "gemini",
          "message": {
            "role": "assistant",
            "content": [
              {
                "kind": "tool_call",
                "data": {
                  "id": "[UUID]",
                  "name": "search",
                  "type": "function",
                  "arguments": {
                    "query": "foo"
                  },
                  "raw_arguments": null,
                  "provider_metadata": {
                    "thoughtSignature": "sig_stream_g"
                  }
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
async fn stream_thought_parts() {
    let sse = support::sse_data_transcript(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Let me think","thought":true}]}}]}"#,
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"4."}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":15,"candidatesTokenCount":12,"thoughtsTokenCount":6}}"#,
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
        "text_id": "[UUID]"
      },
      {
        "type": "text_delta",
        "delta": "4.",
        "text_id": "[UUID]"
      },
      {
        "type": "text_end",
        "text_id": "[UUID]"
      },
      {
        "type": "finish",
        "finish_reason": "stop",
        "usage": {
          "input_tokens": 15,
          "output_tokens": 12,
          "reasoning_tokens": 6,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0
        },
        "response": {
          "id": "[UUID]",
          "model": "gemini-test",
          "provider": "gemini",
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
            "input_tokens": 15,
            "output_tokens": 12,
            "reasoning_tokens": 6,
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

/// The Gemini decoder synthesizes a `Finish` on byte-stream end
/// unconditionally — even when no chunk carried a `finishReason`.
#[tokio::test]
async fn stream_end_synthesizes_finish_without_finish_reason() {
    let sse = support::sse_data_transcript(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}"#,
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events, @r#"
    [
      {
        "type": "stream_start"
      },
      {
        "type": "text_start",
        "text_id": "[UUID]"
      },
      {
        "type": "text_delta",
        "delta": "Hello",
        "text_id": "[UUID]"
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
          "id": "[UUID]",
          "model": "gemini-test",
          "provider": "gemini",
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
