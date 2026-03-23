# unified-llm

A unified async Rust client library for multiple LLM providers. Write your LLM integration code once and switch between Anthropic, OpenAI, and Google Gemini without changing your application logic.

## Key concepts

- **Client** -- Routes requests to registered provider adapters. Can be created explicitly or auto-configured from environment variables.
- **ProviderAdapter** -- The trait every provider implements (`complete` and `stream`). Built-in adapters: `AnthropicAdapter`, `OpenAiAdapter`, `GeminiAdapter`, `OpenAiCompatibleAdapter`.
- **Middleware** -- Intercepts requests/responses for logging, caching, or transformation. Supports both blocking and streaming paths.
- **generate()** -- High-level function that wraps `Client.complete()` with automatic tool execution loops, retries, timeouts, and cancellation.
- **Tool** -- Active tools (with an execute handler) run automatically in the tool loop. Passive tools (no handler) surface tool calls back to the caller.
- **Model catalog** -- Built-in metadata for common models. Advisory only; unknown model strings pass through.

## Providers

| Provider | Adapter | API | Env var |
|----------|---------|-----|---------|
| Anthropic | `AnthropicAdapter` | Messages API | `ANTHROPIC_API_KEY` |
| OpenAI | `OpenAiAdapter` | Responses API | `OPENAI_API_KEY` |
| Google Gemini | `GeminiAdapter` | generateContent | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |
| OpenAI-compatible | `OpenAiCompatibleAdapter` | Chat Completions | (custom) |

All adapters support streaming, tool calling, structured output (`response_format`), and provider-specific options via `provider_options`.

## Usage

### Auto-configure from environment

```rust
use unified_llm::client::Client;
use unified_llm::types::{Message, Request};

let client = Client::from_env().await?;

let request = Request {
    model: "claude-sonnet-4-5".to_string(),
    messages: vec![Message::user("What is the capital of France?")],
    provider: None,
    tools: None,
    tool_choice: None,
    response_format: None,
    temperature: Some(0.0),
    top_p: None,
    max_tokens: Some(100),
    stop_sequences: None,
    reasoning_effort: None,
    metadata: None,
    provider_options: None,
};

let response = client.complete(&request).await?;
println!("{}", response.text());
```

### High-level generate()

```rust
use unified_llm::generate::{generate, GenerateParams};

let result = generate(
    GenerateParams::new("claude-sonnet-4-5")
        .prompt("Explain monads in one sentence")
        .system("You are a concise programming tutor.")
        .max_tokens(200)
).await?;

println!("{}", result.text());
```

### Tool calling

```rust
use unified_llm::generate::{generate, GenerateParams};
use unified_llm::tools::Tool;
use std::sync::Arc;

let weather_tool = Tool::active(
    "get_weather",
    "Get the current weather for a city",
    serde_json::json!({
        "type": "object",
        "properties": {
            "city": {"type": "string", "description": "City name"}
        },
        "required": ["city"]
    }),
    |args, _ctx| async move {
        let city = args["city"].as_str().unwrap_or("unknown");
        Ok(serde_json::json!({"temp": "72F", "city": city}))
    },
);

let result = generate(
    GenerateParams::new("claude-sonnet-4-5")
        .prompt("What's the weather in San Francisco?")
        .tools(vec![weather_tool])
        .max_tool_rounds(3)
).await?;
```

### Streaming

```rust
use unified_llm::client::Client;
use unified_llm::types::{Message, Request, StreamEvent};
use futures::StreamExt;

let client = Client::from_env().await?;
let request = Request {
    model: "claude-sonnet-4-5".to_string(),
    messages: vec![Message::user("Tell me a joke")],
    // ...other fields set to None/defaults
    # provider: None, tools: None, tool_choice: None,
    # response_format: None, temperature: None, top_p: None,
    # max_tokens: None, stop_sequences: None, reasoning_effort: None,
    # metadata: None, provider_options: None,
};

let mut stream = client.stream(&request).await?;
while let Some(event) = stream.next().await {
    match event? {
        StreamEvent::TextDelta { delta, .. } => print!("{delta}"),
        StreamEvent::Finish { response, .. } => {
            println!("\nTokens used: {}", response.usage.total_tokens);
        }
        _ => {}
    }
}
```

### Middleware

```rust
use unified_llm::middleware::{Middleware, NextFn, NextStreamFn};
use unified_llm::types::{Request, Response};
use unified_llm::provider::StreamEventStream;
use unified_llm::error::SdkError;

struct LoggingMiddleware;

#[async_trait::async_trait]
impl Middleware for LoggingMiddleware {
    async fn handle_complete(
        &self,
        request: Request,
        next: NextFn,
    ) -> Result<Response, SdkError> {
        eprintln!("Request to model: {}", request.model);
        let response = next(request).await?;
        eprintln!("Response tokens: {}", response.usage.total_tokens);
        Ok(response)
    }

    async fn handle_stream(
        &self,
        request: Request,
        next: NextStreamFn,
    ) -> Result<StreamEventStream, SdkError> {
        next(request).await
    }
}
```

### OpenAI-compatible providers

```rust
use unified_llm::providers::OpenAiCompatibleAdapter;
use std::sync::Arc;

let adapter = OpenAiCompatibleAdapter::new("your-api-key", "https://api.groq.com/openai/v1")
    .with_name("groq");
```

### Model catalog

```rust
use unified_llm::catalog::{get_model_info, list_models, get_latest_model};

let info = get_model_info("claude-opus-4-6");
let anthropic_models = list_models(Some("anthropic"));
let best_reasoner = get_latest_model("anthropic", Some("reasoning"));
```

## Key types

| Type | Description |
|------|-------------|
| `Request` | Unified request with model, messages, tools, temperature, etc. |
| `Response` | Unified response with message, finish reason, usage, rate limit info |
| `Message` | A message with role, content parts, and optional tool call ID |
| `ContentPart` | Text, Image, Audio, Document, ToolCall, ToolResult, Thinking |
| `StreamEvent` | Events for streaming: TextDelta, ToolCallStart/Delta/End, Finish, etc. |
| `SdkError` | Typed errors with retryability, status codes, and provider error kinds |
| `GenerateParams` | Builder for the high-level `generate()` function |
| `GenerateResult` | Result containing response, tool results, total usage, and step history |
| `ToolDefinition` | Tool name, description, and JSON Schema parameters |
| `ToolChoice` | Auto, None, Required, or Named tool selection |
| `Usage` | Token counts including input, output, reasoning, and cache tokens |
| `RetryPolicy` | Configurable retry with exponential backoff, jitter, and max delay |
| `Model` | Metadata about a model (context window, capabilities, costs) |

## Error handling

`SdkError` provides structured error variants with built-in retryability classification:

- **Retryable**: `RateLimit`, `Server`, `Network`, `Stream`, `RequestTimeout`
- **Non-retryable**: `Authentication`, `AccessDenied`, `InvalidRequest`, `ContextLength`, `Configuration`

The `retry()` function and `generate()` respect `Retry-After` headers and use exponential backoff with jitter.

## Provider-specific options

Pass provider-specific parameters via `provider_options` without losing portability:

```rust
use unified_llm::types::Request;

let request = Request {
    provider_options: Some(serde_json::json!({
        "anthropic": {
            "thinking": {"type": "enabled", "budget_tokens": 10000},
            "auto_cache": true
        },
        "openai": {
            "store": true,
            "previous_response_id": "resp_abc123"
        },
        "gemini": {
            "safetySettings": [
                {"category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_NONE"}
            ]
        }
    })),
    // ...other fields
    # model: String::new(), messages: vec![], provider: None, tools: None,
    # tool_choice: None, response_format: None, temperature: None,
    # top_p: None, max_tokens: None, stop_sequences: None,
    # reasoning_effort: None, metadata: None,
};
```
